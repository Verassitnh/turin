//! Port of the installer-probe half of upstream/packages/provider-core/src/server/maintenanceResolver.ts:
//! package-manager ownership (npm, Homebrew, pnpm, yarn, bun, vite-plus, volta,
//! mise), the cached resolution, and the latest-version lookup.
//!
//! The pure path and layout rules are plain functions. Anything that touches the
//! filesystem or runs a probe is `async`, and every probe is bounded: a timed-out
//! `brew` is killed with its process group rather than left running.

use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, Instant},
};

use regex::Regex;
use serde::Deserialize;
use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex};
use turin_contracts::{UtcDateTime, provider_instance::ProviderDriverKind};

use crate::{
    command_path::resolve_command_path,
    host_process::{Platform, ProcessEnv, RedactedEnvironment},
    maintenance::{ProviderMaintenanceCapabilities, ProviderMaintenanceCommandAction},
    maintenance_resolver::{
        ProviderVersionAdvisoryInput, create_provider_version_advisory,
        make_manual_only_provider_maintenance_capabilities,
    },
    provider_latest_versions::ProviderLatestVersions,
    server_provider::ServerProvider,
};

const LATEST_VERSION_TIMEOUT: Duration = Duration::from_secs(4);
const HOMEBREW_INFO_TIMEOUT: Duration = Duration::from_secs(10);
const HOMEBREW_INFO_MAX_BYTES: usize = 256 * 1024;
const MISE_WRAPPER_MAX_BYTES: u64 = 16 * 1024;
const SIGKILL: i32 = 9;
/// Ownership is re-derived from the executable this often.
pub const MAINTENANCE_CAPABILITIES_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

static MISE_TOOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/mise/installs/([^/]+)/[^/]+$").expect("valid regex"));
static HOMEBREW_KEG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(.*)/(cellar|caskroom)/([^/]+)/[^/]+/").expect("valid regex")
});
static MISE_INVOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bmise\s+(?:x|exec)\b").expect("valid regex"));

/// What the resolver knows about the executable: `resolvedCommandPath` follows
/// `PATH` and symlinks are resolved in `realCommandPath`.
#[derive(Clone)]
pub struct ProviderMaintenanceResolutionContext {
    pub binary_path: String,
    pub resolved_command_path: String,
    pub real_command_path: String,
    pub env: ProcessEnv,
    pub platform: Platform,
}

impl std::fmt::Debug for ProviderMaintenanceResolutionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderMaintenanceResolutionContext")
            .field("binary_path", &self.binary_path)
            .field("resolved_command_path", &self.resolved_command_path)
            .field("real_command_path", &self.real_command_path)
            .field("env", &RedactedEnvironment(&self.env))
            .field("platform", &self.platform)
            .finish()
    }
}

/// The provider's own updater (`claude update`, `codex update`). It runs first
/// for paths its installer owns.
#[derive(Debug, Clone)]
pub struct NativeUpdate {
    pub args: Vec<String>,
    /// Paths the provider's own installer owns; `None` when it has none.
    pub is_command_path: Option<fn(&str) -> bool>,
    pub env: Option<ProcessEnv>,
}

#[derive(Debug, Clone)]
pub struct PackageManagedProviderMaintenanceDefinition {
    pub provider: ProviderDriverKind,
    pub npm_package_name: String,
    pub native_update: Option<NativeUpdate>,
}

/// `normalizeCommandPath`: slashes unified and lowercased, for layout checks.
pub fn normalize_command_path(command_path: &str) -> String {
    command_path.replace('\\', "/").to_ascii_lowercase()
}

fn is_vite_plus_global_command_path(command_path: &str) -> bool {
    normalize_command_path(command_path).contains("/.vite-plus/bin/")
}

fn is_bun_global_command_path(command_path: &str) -> bool {
    normalize_command_path(command_path).contains("/.bun/bin/")
}

fn is_yarn_global_command_path(command_path: &str) -> bool {
    // `~/.config/yarn/global/…` on POSIX, `%LOCALAPPDATA%\Yarn\Data\global\…` on Windows.
    Regex::new(r"/yarn/(?:data/)?global/node_modules/")
        .expect("valid regex")
        .is_match(&normalize_command_path(command_path))
}

/// Version-manager installs are pinned in its config, so updating them means editing that.
fn is_mise_command_path(command_path: &str) -> bool {
    let normalized = normalize_command_path(command_path);
    normalized.contains("/mise/installs/") || normalized.contains("/mise/shims/")
}

fn is_pnpm_global_command_path(command_path: &str) -> bool {
    let normalized = normalize_command_path(command_path);
    [
        "/.local/share/pnpm/",
        "/library/pnpm/",
        "/local/share/pnpm/",
        "/appdata/local/pnpm/",
        "/pnpm/global/",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

/// The npm global prefix that owns a package, from the real path of its entry
/// point (`<prefix>/lib/node_modules/<pkg>/…`). A project-local `node_modules`
/// or a copy nested inside another package is not a global install.
pub fn npm_global_prefix_from_command_path(
    real_command_path: &str,
    package_name: &str,
) -> Option<String> {
    let slash_path = real_command_path.replace('\\', "/");
    // ASCII lowercasing keeps indices aligned with `slash_path`.
    let normalized = slash_path.to_ascii_lowercase();
    let package_segment = format!("/lib/node_modules/{}/", package_name.to_ascii_lowercase());
    let package_index = normalized.rfind(&package_segment)?;
    let before = &normalized[..package_index];
    if before.contains("/node_modules/") {
        return None;
    }
    // Mise's npm backend uses a global-looking layout inside a tool version.
    // Globals under its Node installation still belong to npm.
    let mise_tool = MISE_TOOL
        .captures(before)
        .map(|captures| captures[1].to_owned());
    if mise_tool.is_some_and(|tool| tool != "node") {
        return None;
    }
    Some(if package_index == 0 {
        "/".to_owned()
    } else {
        slash_path[..package_index].to_owned()
    })
}

/// A Homebrew keg or cask, from the real path of the executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomebrewOwnership {
    pub kind: HomebrewKind,
    pub name: String,
    /// The Homebrew prefix the keg sits under; must match `brew --prefix`.
    pub prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomebrewKind {
    Formula,
    Cask,
}

/// Homebrew owns a path that runs through a versioned keg or cask. Ownership is
/// only proven once the prefix matches the `brew` that will run the upgrade.
pub fn homebrew_ownership_from_command_path(real_command_path: &str) -> Option<HomebrewOwnership> {
    let slash_path = real_command_path.replace('\\', "/");
    let captures = HOMEBREW_KEG.captures(&slash_path)?;
    Some(HomebrewOwnership {
        kind: if captures[2].eq_ignore_ascii_case("cellar") {
            HomebrewKind::Formula
        } else {
            HomebrewKind::Cask
        },
        name: captures[3].to_owned(),
        prefix: captures[1].to_owned(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HomebrewInfo {
    formulae: Option<Vec<HomebrewFormula>>,
    casks: Option<Vec<HomebrewCask>>,
}

#[derive(Deserialize)]
struct HomebrewFormula {
    versions: Option<HomebrewVersions>,
}

#[derive(Deserialize)]
struct HomebrewVersions {
    stable: Option<String>,
}

#[derive(Deserialize)]
struct HomebrewCask {
    version: Option<String>,
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    let trimmed = turin_contracts::js_trim(value?);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The latest version `brew info --json=v2` reports for the keg. Cask versions
/// may carry a build suffix after a comma (`1.2.3,456`), which is dropped.
pub fn parse_homebrew_latest_version(
    info_json: &str,
    ownership: &HomebrewOwnership,
) -> Option<String> {
    let info: HomebrewInfo = serde_json::from_str(info_json).ok()?;
    let raw = match ownership.kind {
        HomebrewKind::Formula => info
            .formulae
            .as_ref()
            .and_then(|formulae| formulae.first())
            .and_then(|formula| formula.versions.as_ref())
            .and_then(|versions| versions.stable.as_deref())
            .map(str::to_owned),
        HomebrewKind::Cask => info
            .casks
            .as_ref()
            .and_then(|casks| casks.first())
            .and_then(|cask| cask.version.as_deref())
            .map(|version| version.split(',').next().unwrap_or_default().to_owned()),
    };
    non_empty_string(raw.as_deref())
}

/// `quoteShellWord`: quotes a word the host's default shell would split.
fn quote_shell_word(word: &str, platform: Platform) -> String {
    let safe = if platform == Platform::Win32 {
        Regex::new(r"^[\w./:\\@=-]+$")
    } else {
        Regex::new(r"^[\w./:@=-]+$")
    }
    .expect("valid regex");
    if safe.is_match(word) {
        return word.to_owned();
    }
    if platform == Platform::Win32 {
        let doubled: String = word
            .chars()
            .flat_map(|ch| {
                if matches!(ch, '\'' | '\u{2018}' | '\u{2019}') {
                    vec![ch, ch]
                } else {
                    vec![ch]
                }
            })
            .collect();
        format!("'{doubled}'")
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// Windows terminals default to PowerShell, where a quoted executable needs `&`.
fn quote_update_executable(executable: &str, platform: Platform) -> String {
    let quoted = quote_shell_word(executable, platform);
    if platform == Platform::Win32 && quoted != executable {
        format!("& {quoted}")
    } else {
        quoted
    }
}

/// `makeProviderMaintenanceCapabilities`: an update action only when both the
/// executable and the lock key are known.
#[derive(Debug, Clone, Default)]
pub struct MaintenanceCapabilitiesInput {
    pub provider: Option<ProviderDriverKind>,
    pub package_name: Option<String>,
    pub update_executable: Option<String>,
    pub update_args: Vec<String>,
    pub update_lock_key: Option<String>,
    /// Shown to the user instead of `<executable> <args>`, for a bare tool name like `brew`.
    pub update_command: Option<String>,
    pub platform: Option<Platform>,
    pub env: Option<ProcessEnv>,
    /// `Some(None)` means the installer was asked and did not know.
    pub latest_version: Option<Option<String>>,
}

pub fn make_provider_maintenance_capabilities(
    provider: ProviderDriverKind,
    input: MaintenanceCapabilitiesInput,
) -> ProviderMaintenanceCapabilities {
    let platform = input.platform.unwrap_or_else(Platform::current);
    let update = match (input.update_executable, input.update_lock_key) {
        (Some(executable), Some(lock_key)) => {
            let command = input.update_command.clone().unwrap_or_else(|| {
                std::iter::once(quote_update_executable(&executable, platform))
                    .chain(
                        input
                            .update_args
                            .iter()
                            .map(|arg| quote_shell_word(arg, platform)),
                    )
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            Some(ProviderMaintenanceCommandAction {
                command,
                executable,
                args: input.update_args,
                lock_key,
                env: input.env,
            })
        }
        _ => None,
    };
    ProviderMaintenanceCapabilities {
        provider,
        package_name: input.package_name,
        update,
        latest_version: input.latest_version,
        compare_versions: None,
    }
}

/// Runs `brew <args>` and returns stdout, or `None` on failure, timeout, or
/// oversized output. The child leads its own process group, so the timeout
/// kills the whole tree, not just `brew`.
async fn run_homebrew(brew_path: &str, args: &[&str], env: &ProcessEnv) -> Option<String> {
    run_homebrew_within(brew_path, args, env, HOMEBREW_INFO_TIMEOUT).await
}

async fn run_homebrew_within(
    brew_path: &str,
    args: &[&str],
    env: &ProcessEnv,
    timeout: Duration,
) -> Option<String> {
    let mut command = tokio::process::Command::new(brew_path);
    command
        .args(args)
        .envs(env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().ok()?;
    let group = child.id().map(i64::from);
    let guard = GroupKill(group);

    let collect = async {
        let mut stdout = child.stdout.take()?;
        let mut bytes = Vec::new();
        let read = (&mut stdout)
            .take(HOMEBREW_INFO_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        let status = child.wait().await.ok()?;
        (status.success() && read <= HOMEBREW_INFO_MAX_BYTES)
            .then(|| String::from_utf8_lossy(&bytes).into_owned())
    };
    let output = tokio::time::timeout(timeout, collect).await.ok().flatten();
    // Only a finished probe disarms the guard. A timeout drops `collect`, and
    // the group kill must still reach brew's children.
    if output.is_some() {
        guard.disarm();
    } else {
        tracing::warn!(
            subcommand = args.first().copied().unwrap_or_default(),
            "Homebrew probe failed"
        );
    }
    output
}

/// Kills a probe's process group when dropped, unless it finished normally.
struct GroupKill(Option<i64>);

impl GroupKill {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for GroupKill {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = crate::process_group::signal_process_group(pid, SIGKILL);
        }
    }
}

/// POSIX npm links `<prefix>/bin/<cmd>` into the package, so the real path is
/// proof. Windows npm writes shims beside `node_modules`, so the package
/// manifest next to the shim is the proof there.
async fn resolve_npm_global_prefix(
    context: &ProviderMaintenanceResolutionContext,
    package_name: &str,
) -> Option<String> {
    if let Some(prefix) =
        npm_global_prefix_from_command_path(&context.real_command_path, package_name)
    {
        return Some(prefix);
    }
    if context.platform != Platform::Win32 {
        return None;
    }
    let shim_dir = Path::new(&context.resolved_command_path)
        .parent()?
        .to_path_buf();
    let mut manifest = shim_dir.join("node_modules");
    for segment in package_name.split('/') {
        manifest.push(segment);
    }
    manifest.push("package.json");
    // A POSIX project checkout has the same shape, which is why this is Windows-only.
    tokio::fs::try_exists(&manifest)
        .await
        .unwrap_or(false)
        .then(|| shim_dir.to_string_lossy().into_owned())
}

/// Volta's `bin/<cmd>` links to its `volta-shim`, which picks the package at
/// run time; the package's image directory proves Volta installed it.
async fn is_volta_package_install(
    context: &ProviderMaintenanceResolutionContext,
    package_name: &str,
) -> bool {
    let normalized = normalize_command_path(&context.real_command_path);
    let basename = Path::new(&normalized)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if basename.strip_suffix(".exe").unwrap_or(&basename) != "volta-shim" {
        return false;
    }
    let Some(volta_home) = Path::new(&context.resolved_command_path)
        .parent()
        .and_then(Path::parent)
    else {
        return false;
    };
    let mut package_dir: PathBuf = volta_home.join("tools").join("image").join("packages");
    for segment in package_name.split('/') {
        package_dir.push(segment);
    }
    tokio::fs::try_exists(package_dir).await.unwrap_or(false)
}

/// A launcher script that runs the provider through mise (`exec mise x codex -- codex`).
async fn is_mise_wrapper_script(context: &ProviderMaintenanceResolutionContext) -> bool {
    let Ok(metadata) = tokio::fs::metadata(&context.real_command_path).await else {
        return false;
    };
    if metadata.len() > MISE_WRAPPER_MAX_BYTES {
        return false;
    }
    let script = tokio::fs::read_to_string(&context.real_command_path)
        .await
        .unwrap_or_default();
    script.starts_with("#!") && MISE_INVOCATION.is_match(&script)
}

/// Derives update capabilities from where the executable actually lives. Every
/// package-manager branch has evidence that the named tool owns that path, so
/// T3 Code never runs a package manager against an install it did not create.
/// An unproven install falls back to the provider's own updater, and stays
/// manual-only without one.
pub async fn resolve_package_managed_provider_maintenance(
    definition: &PackageManagedProviderMaintenanceDefinition,
    context: Option<&ProviderMaintenanceResolutionContext>,
) -> ProviderMaintenanceCapabilities {
    let package_name = definition.npm_package_name.as_str();
    let manual = make_manual_only_provider_maintenance_capabilities(definition.provider.clone());
    let manual = ProviderMaintenanceCapabilities {
        package_name: Some(package_name.to_owned()),
        ..manual
    };
    let Some(context) = context else {
        return manual;
    };
    let command_paths = [
        context.resolved_command_path.as_str(),
        context.real_command_path.as_str(),
    ];
    let package = |executable: &str, args: Vec<String>, lock_key: &str| {
        make_provider_maintenance_capabilities(
            definition.provider.clone(),
            MaintenanceCapabilitiesInput {
                package_name: Some(package_name.to_owned()),
                update_executable: Some(executable.to_owned()),
                update_args: args,
                update_lock_key: Some(lock_key.to_owned()),
                platform: Some(context.platform),
                ..MaintenanceCapabilitiesInput::default()
            },
        )
    };

    let native = match &definition.native_update {
        Some(native_update) => make_provider_maintenance_capabilities(
            definition.provider.clone(),
            MaintenanceCapabilitiesInput {
                package_name: Some(package_name.to_owned()),
                update_executable: Some(context.resolved_command_path.clone()),
                update_args: native_update.args.clone(),
                update_lock_key: Some(format!("{}-native", definition.provider.as_str())),
                platform: Some(context.platform),
                env: native_update.env.clone(),
                ..MaintenanceCapabilitiesInput::default()
            },
        ),
        None => manual.clone(),
    };
    if let Some(native_update) = &definition.native_update
        && let Some(is_command_path) = native_update.is_command_path
        && command_paths.iter().any(|path| is_command_path(path))
    {
        return native;
    }
    // A `node_modules` path not proven below belongs to another package or a
    // project, so the provider's own updater could act on the wrong install.
    let fallback = if command_paths
        .iter()
        .any(|path| normalize_command_path(path).contains("/node_modules/"))
    {
        manual.clone()
    } else {
        native
    };

    if command_paths
        .iter()
        .any(|path| is_vite_plus_global_command_path(path))
    {
        return package(
            "vp",
            vec!["i".into(), "-g".into(), package_name.into()],
            "vite-plus-global",
        );
    }
    if command_paths
        .iter()
        .any(|path| is_bun_global_command_path(path))
    {
        return package(
            "bun",
            vec!["i".into(), "-g".into(), format!("{package_name}@latest")],
            "bun-global",
        );
    }
    if command_paths
        .iter()
        .any(|path| is_pnpm_global_command_path(path))
    {
        return package(
            "pnpm",
            vec!["add".into(), "-g".into(), format!("{package_name}@latest")],
            "pnpm-global",
        );
    }
    if command_paths
        .iter()
        .any(|path| is_yarn_global_command_path(path))
    {
        return package(
            "yarn",
            vec![
                "global".into(),
                "add".into(),
                format!("{package_name}@latest"),
            ],
            "yarn-global",
        );
    }
    if is_volta_package_install(context, package_name).await {
        return package(
            "volta",
            vec!["install".into(), format!("{package_name}@latest")],
            "volta",
        );
    }

    // npm proof names the package, so it outranks a keg the path merely passes
    // through: a Homebrew-installed Node keeps its globals under
    // `Cellar/node/<ver>/lib/node_modules/`, and that is npm's install.
    if let Some(npm_prefix) = resolve_npm_global_prefix(context, package_name).await {
        // npm 12 blocks install scripts by default and still exits 0, so a
        // package whose postinstall finishes the install is left broken. Allow
        // this one package's scripts; older npm warns and continues.
        let lock_key = format!("npm-global:{}", normalize_command_path(&npm_prefix));
        return package(
            "npm",
            vec![
                "install".into(),
                "-g".into(),
                "--prefix".into(),
                npm_prefix,
                format!("--allow-scripts={package_name}"),
                format!("{package_name}@latest"),
            ],
            &lock_key,
        );
    }

    if command_paths.iter().any(|path| is_mise_command_path(path))
        || is_mise_wrapper_script(context).await
    {
        return manual;
    }

    let Some(homebrew) = homebrew_ownership_from_command_path(&context.real_command_path) else {
        return fallback;
    };
    // Mise shims resolve to the version manager, not the provider.
    if homebrew.kind == HomebrewKind::Formula && homebrew.name.eq_ignore_ascii_case("mise") {
        return manual;
    }
    // The keg's own brew works even when a GUI-launched server has no Homebrew
    // on PATH; the prefix check below still applies to it.
    let keg_brew = Path::new(&homebrew.prefix).join("bin").join("brew");
    let brew_path = if tokio::fs::try_exists(&keg_brew).await.unwrap_or(false) {
        Some(keg_brew.to_string_lossy().into_owned())
    } else {
        resolve_command_path("brew", &context.env, context.platform)
    };
    let Some(brew_path) = brew_path else {
        return fallback;
    };
    // A keg-shaped path is only Homebrew's if it sits under the prefix of the
    // `brew` that would upgrade it; `brew --prefix` is a cheap shell script.
    let Some(brew_prefix) = non_empty_string(
        run_homebrew(&brew_path, &["--prefix"], &context.env)
            .await
            .as_deref(),
    ) else {
        return fallback;
    };
    let real_brew_prefix = tokio::fs::canonicalize(&brew_prefix)
        .await
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or(brew_prefix);
    if normalize_command_path(&real_brew_prefix) != normalize_command_path(&homebrew.prefix) {
        return fallback;
    }
    let args: Vec<String> = match homebrew.kind {
        HomebrewKind::Cask => vec!["upgrade".into(), "--cask".into(), homebrew.name.clone()],
        HomebrewKind::Formula => vec!["upgrade".into(), homebrew.name.clone()],
    };
    // Homebrew lags npm by hours on every release, so compare against what
    // `brew upgrade` can actually deliver.
    let info = run_homebrew(
        &brew_path,
        &["info", "--json=v2", &homebrew.name],
        &context.env,
    )
    .await;
    let latest_version = info
        .as_deref()
        .and_then(|json| parse_homebrew_latest_version(json, &homebrew));
    make_provider_maintenance_capabilities(
        definition.provider.clone(),
        MaintenanceCapabilitiesInput {
            package_name: Some(package_name.to_owned()),
            update_executable: Some(brew_path),
            update_args: args.clone(),
            update_lock_key: Some("homebrew".into()),
            update_command: Some(
                ["brew".to_owned()]
                    .into_iter()
                    .chain(args)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            platform: Some(context.platform),
            latest_version: Some(latest_version),
            ..MaintenanceCapabilitiesInput::default()
        },
    )
}

/// Reads `PATH`-style lookup variables the way upstream's `CommandLookupEnvConfig` does.
pub fn command_lookup_env() -> ProcessEnv {
    ["PATH", "Path", "path", "PATHEXT"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
        .collect()
}

/// `resolveProviderMaintenanceCapabilitiesEffect`: finds the configured
/// executable, follows symlinks, and hands the result to the resolver. A binary
/// that cannot be found resolves with no context.
pub async fn resolve_provider_maintenance_capabilities(
    definition: &PackageManagedProviderMaintenanceDefinition,
    binary_path: Option<&str>,
    env: Option<ProcessEnv>,
    platform: Platform,
) -> ProviderMaintenanceCapabilities {
    let Some(binary_path) = non_empty_string(binary_path) else {
        return resolve_package_managed_provider_maintenance(definition, None).await;
    };
    let env = env.unwrap_or_else(command_lookup_env);
    // `resolve_command_path` checks explicit paths for existence too, so a
    // missing binary never gets an update command it cannot run.
    let Some(resolved_command_path) = resolve_command_path(&binary_path, &env, platform) else {
        return resolve_package_managed_provider_maintenance(definition, None).await;
    };
    let Ok(real) = tokio::fs::canonicalize(&resolved_command_path).await else {
        return resolve_package_managed_provider_maintenance(definition, None).await;
    };
    let context = ProviderMaintenanceResolutionContext {
        binary_path,
        real_command_path: real.to_string_lossy().into_owned(),
        resolved_command_path,
        env,
        platform,
    };
    resolve_package_managed_provider_maintenance(definition, Some(&context)).await
}

/// Caches a resolution for an hour. `fresh` bypasses the cache, so an update
/// never trusts ownership derived before the user asked for it. A resolution
/// that is cancelled is not cached.
pub struct CachedMaintenanceResolution {
    definition: PackageManagedProviderMaintenanceDefinition,
    binary_path: Option<String>,
    env: Option<ProcessEnv>,
    platform: Platform,
    cached: AsyncMutex<Option<(Instant, ProviderMaintenanceCapabilities)>>,
}

impl CachedMaintenanceResolution {
    pub fn new(
        definition: PackageManagedProviderMaintenanceDefinition,
        binary_path: Option<String>,
        env: Option<ProcessEnv>,
        platform: Platform,
    ) -> Self {
        Self {
            definition,
            binary_path,
            env,
            platform,
            cached: AsyncMutex::new(None),
        }
    }

    pub async fn get(&self, fresh: bool) -> ProviderMaintenanceCapabilities {
        let mut cached = self.cached.lock().await;
        if !fresh
            && let Some((resolved_at, capabilities)) = cached.as_ref()
            && resolved_at.elapsed() < MAINTENANCE_CAPABILITIES_CACHE_TTL
        {
            return capabilities.clone();
        }
        *cached = None;
        let capabilities = resolve_provider_maintenance_capabilities(
            &self.definition,
            self.binary_path.as_deref(),
            self.env.clone(),
            self.platform,
        )
        .await;
        *cached = Some((Instant::now(), capabilities.clone()));
        capabilities
    }
}

/// `encodeURIComponent` for a package name: scoped names become `%40scope%2Fname`.
fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[derive(Deserialize)]
struct NpmLatestVersion {
    version: Option<String>,
}

/// The npm registry's `latest` version for a package, or `None` on timeout,
/// non-2xx status, or an unreadable body.
pub async fn fetch_npm_latest_version(
    client: &reqwest::Client,
    package_name: &str,
) -> Option<String> {
    let url = format!(
        "https://registry.npmjs.org/{}/latest",
        encode_uri_component(package_name)
    );
    let response = tokio::time::timeout(
        LATEST_VERSION_TIMEOUT,
        client.get(url).header("accept", "application/json").send(),
    )
    .await
    .ok()?
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let payload: NpmLatestVersion = response.json().await.ok()?;
    non_empty_string(payload.version.as_deref())
}

/// The latest version to compare against: the installer's own answer when it
/// has one, otherwise the npm registry through the shared cache.
pub async fn resolve_latest_provider_version(
    capabilities: &ProviderMaintenanceCapabilities,
    latest_versions: &ProviderLatestVersions,
    client: &reqwest::Client,
    fresh: bool,
) -> Option<String> {
    if let Some(latest) = &capabilities.latest_version {
        return latest.clone();
    }
    let package_name = capabilities.package_name.as_deref()?;
    let fetched = latest_versions
        .cached(
            package_name,
            || async {
                Ok::<_, std::convert::Infallible>(
                    fetch_npm_latest_version(client, package_name).await,
                )
            },
            fresh,
        )
        .await;
    fetched.ok().flatten()
}

/// `enrichProviderSnapshotWithVersionAdvisory`: attaches the advisory, looking
/// up the latest version only when update checks are on and the install is live.
pub async fn enrich_provider_snapshot_with_version_advisory(
    mut snapshot: ServerProvider,
    capabilities: &ProviderMaintenanceCapabilities,
    enable_provider_update_checks: Option<bool>,
    latest_versions: &ProviderLatestVersions,
    client: &reqwest::Client,
) -> ServerProvider {
    let should_resolve = enable_provider_update_checks != Some(false)
        && snapshot.enabled
        && snapshot.installed
        && snapshot.version.is_some();
    let latest = if should_resolve {
        resolve_latest_provider_version(capabilities, latest_versions, client, false).await
    } else {
        None
    };
    let current = snapshot
        .version
        .as_ref()
        .map(|version| version.as_str().to_owned());
    // A fresh lookup is stamped now; otherwise the advisory keeps the snapshot's own check time.
    let checked_at = if should_resolve {
        Some(UtcDateTime::now().to_string())
    } else {
        Some(snapshot.checked_at.to_string())
    };
    snapshot.version_advisory = Some(create_provider_version_advisory(
        ProviderVersionAdvisoryInput {
            driver: &snapshot.driver,
            current_version: current.as_deref(),
            latest_version: latest.as_deref(),
            checked_at: checked_at.as_deref(),
            maintenance: Some(capabilities),
        },
    ));
    snapshot
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn codex_definition() -> PackageManagedProviderMaintenanceDefinition {
        PackageManagedProviderMaintenanceDefinition {
            provider: ProviderDriverKind::new("codex").unwrap(),
            npm_package_name: "@openai/codex".into(),
            native_update: None,
        }
    }

    #[test]
    fn derives_the_npm_prefix_only_from_the_global_lib_node_modules_layout() {
        assert_eq!(
            npm_global_prefix_from_command_path(
                "/usr/local/lib/node_modules/@openai/codex/bin/codex.js",
                "@openai/codex"
            )
            .as_deref(),
            Some("/usr/local")
        );
        assert_eq!(
            npm_global_prefix_from_command_path(
                "/usr/local/lib/node_modules/other/node_modules/@openai/codex/bin/codex.js",
                "@openai/codex"
            ),
            None
        );
        assert_eq!(
            npm_global_prefix_from_command_path(
                "/lib/node_modules/@openai/codex/bin/codex.js",
                "@openai/codex"
            )
            .as_deref(),
            Some("/")
        );
        assert_eq!(
            npm_global_prefix_from_command_path(
                "/work/app/node_modules/@openai/codex/bin/codex.js",
                "@openai/codex"
            ),
            None
        );
    }

    #[test]
    fn recognizes_homebrew_kegs_and_casks_from_the_real_path() {
        assert_eq!(
            homebrew_ownership_from_command_path("/opt/homebrew/Cellar/codex/0.9.0/bin/codex"),
            Some(HomebrewOwnership {
                kind: HomebrewKind::Formula,
                name: "codex".into(),
                prefix: "/opt/homebrew".into(),
            })
        );
        assert_eq!(
            homebrew_ownership_from_command_path("/opt/homebrew/Caskroom/claude/1.2.3/claude"),
            Some(HomebrewOwnership {
                kind: HomebrewKind::Cask,
                name: "claude".into(),
                prefix: "/opt/homebrew".into(),
            })
        );
        assert_eq!(
            homebrew_ownership_from_command_path("/usr/local/bin/codex"),
            None
        );
    }

    #[test]
    fn reads_the_stable_formula_version_and_drops_the_cask_build_suffix() {
        let formula =
            homebrew_ownership_from_command_path("/opt/homebrew/Cellar/codex/0.9.0/bin/codex")
                .unwrap();
        assert_eq!(
            parse_homebrew_latest_version(
                r#"{"formulae":[{"versions":{"stable":"0.10.0"}}]}"#,
                &formula
            )
            .as_deref(),
            Some("0.10.0")
        );
        let cask =
            homebrew_ownership_from_command_path("/opt/homebrew/Caskroom/claude/1.2.3/claude")
                .unwrap();
        assert_eq!(
            parse_homebrew_latest_version(r#"{"casks":[{"version":"1.2.4,456"}]}"#, &cask)
                .as_deref(),
            Some("1.2.4")
        );
        assert_eq!(parse_homebrew_latest_version("not json", &formula), None);
    }

    #[test]
    fn quotes_copyable_command_words_the_shell_would_split() {
        assert_eq!(
            quote_shell_word("plain-word", Platform::Darwin),
            "plain-word"
        );
        assert_eq!(
            quote_shell_word("it's here", Platform::Darwin),
            "'it'\\''s here'"
        );
        assert_eq!(
            quote_update_executable("C:\\Program Files\\x.exe", Platform::Win32),
            "& 'C:\\Program Files\\x.exe'"
        );
    }

    #[test]
    fn package_manager_actions_pin_the_package_and_lock_on_the_prefix() {
        let capabilities = make_provider_maintenance_capabilities(
            ProviderDriverKind::new("codex").unwrap(),
            MaintenanceCapabilitiesInput {
                package_name: Some("@openai/codex".into()),
                update_executable: Some("npm".into()),
                update_args: vec!["install".into(), "-g".into(), "@openai/codex@latest".into()],
                update_lock_key: Some("npm-global:/usr/local".into()),
                platform: Some(Platform::Darwin),
                ..MaintenanceCapabilitiesInput::default()
            },
        );
        let update = capabilities.update.unwrap();
        assert_eq!(update.command, "npm install -g @openai/codex@latest");
        assert_eq!(update.lock_key, "npm-global:/usr/local");
    }

    #[test]
    fn encodes_scoped_package_names_like_encode_uri_component() {
        assert_eq!(encode_uri_component("@openai/codex"), "%40openai%2Fcodex");
        assert_eq!(encode_uri_component("opencode-ai"), "opencode-ai");
    }

    #[tokio::test]
    async fn an_npm_global_install_is_updated_with_npm_pinned_to_its_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir
            .path()
            .join("lib/node_modules/@openai/codex/bin/codex.js");
        let context = ProviderMaintenanceResolutionContext {
            binary_path: "codex".into(),
            resolved_command_path: dir.path().join("bin/codex").to_string_lossy().into_owned(),
            real_command_path: entry.to_string_lossy().into_owned(),
            env: ProcessEnv::new(),
            platform: Platform::Darwin,
        };
        let capabilities =
            resolve_package_managed_provider_maintenance(&codex_definition(), Some(&context)).await;
        let update = capabilities.update.expect("npm owns the install");
        assert_eq!(update.executable, "npm");
        let prefix = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            update.args,
            [
                "install",
                "-g",
                "--prefix",
                prefix.as_str(),
                "--allow-scripts=@openai/codex",
                "@openai/codex@latest"
            ]
        );
    }

    #[tokio::test]
    async fn an_unproven_project_install_stays_manual_only() {
        let context = ProviderMaintenanceResolutionContext {
            binary_path: "codex".into(),
            resolved_command_path: "/work/app/node_modules/.bin/codex".into(),
            real_command_path: "/work/app/node_modules/@openai/codex/bin/codex.js".into(),
            env: ProcessEnv::new(),
            platform: Platform::Darwin,
        };
        let capabilities =
            resolve_package_managed_provider_maintenance(&codex_definition(), Some(&context)).await;
        assert_eq!(capabilities.update, None);
    }

    #[tokio::test]
    async fn a_missing_binary_resolves_to_manual_only_with_the_package_name() {
        let capabilities =
            resolve_package_managed_provider_maintenance(&codex_definition(), None).await;
        assert_eq!(capabilities.update, None);
        assert_eq!(capabilities.package_name.as_deref(), Some("@openai/codex"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_probe_takes_its_grandchildren_down_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let pid_file_arg = pid_file.to_str().unwrap();
        // `sleep` is brew's child, so only the group kill can reach it.
        let output = run_homebrew_within(
            "/bin/sh",
            &["-c", "sleep 600 & echo $! > \"$0\"; wait", pid_file_arg],
            &ProcessEnv::new(),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(output, None);

        let grandchild: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        crate::process_group::assert_process_exits(grandchild, "grandchild").await;
    }

    #[test]
    fn resolution_context_debug_redacts_environment_values() {
        let context = ProviderMaintenanceResolutionContext {
            binary_path: "/usr/local/bin/codex".into(),
            resolved_command_path: "/usr/local/bin/codex".into(),
            real_command_path: "/usr/local/bin/codex".into(),
            env: ProcessEnv::from([("OPENAI_API_KEY".to_owned(), "fixture-key".to_owned())]),
            platform: Platform::Darwin,
        };
        let debug = format!("{context:?}");
        assert!(debug.contains("OPENAI_API_KEY"), "{debug}");
        assert!(!debug.contains("fixture-key"), "{debug}");
    }
}

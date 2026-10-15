//! Port of upstream/packages/shared/src/shell.ts
//!
//! Login-shell PATH hydration, Windows environment probing, and command
//! lookup. Upstream reaches the host through `HostProcess.Platform` /
//! `HostProcess.Environment` and `Context.Reference` overrides; here the
//! platform, environment, resolver, cache, and clock are explicit parameters
//! so tests inject them the same way.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use parking_lot::Mutex;
use turin_contracts::base_schemas::js_trim;

/// `NodeJS.ProcessEnv`. Insertion order is kept so merged environments list
/// keys the way upstream's object spread does.
pub type ShellEnv = IndexMap<String, String>;

/// `NodeJS.Platform`, reduced to the cases this module branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Win32,
    Darwin,
    Linux,
    Other,
}

impl Platform {
    /// The platform this binary runs on.
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Win32
        } else if cfg!(target_os = "macos") {
            Platform::Darwin
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }

    fn is_win32(self) -> bool {
        self == Platform::Win32
    }
}

const WINDOWS_SHELL_CANDIDATES: [&str; 2] = ["pwsh.exe", "powershell.exe"];
const WINDOWS_PATH_DELIMITER: char = ';';
const POSIX_PATH_DELIMITER: char = ':';

/// `ExecFileSyncLike`: runs `file` with `args` and returns its stdout, or
/// fails on a non-zero exit, a missing binary, or a timeout.
pub type ExecFileSync<'a> = &'a dyn Fn(&str, &[&str], Duration) -> io::Result<String>;

/// `execFileSync(file, args, { encoding: "utf8", timeout })`. A child that
/// outlives `timeout` is killed rather than left running.
pub fn exec_file_sync(file: &str, args: &[&str], timeout: Duration) -> io::Result<String> {
    let mut child = Command::new(file)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        if let Some(mut stdout) = stdout {
            stdout.read_to_end(&mut output)?;
        }
        Ok::<_, io::Error>(output)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let output = reader
        .join()
        .map_err(|_| io::Error::other("stdout reader panicked"))??;
    if !status.success() {
        return Err(io::Error::other(format!("{file} exited with {status}")));
    }
    String::from_utf8(output).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// `Escapes a single argument for cmd.exe shell mode`. Mirrors cross-spawn.
pub fn escape_windows_shell_arg(arg: &str) -> String {
    // Double up backslashes that precede a double quote, then escape the quote
    // itself so it survives CommandLineToArgvW.
    let mut escaped = String::with_capacity(arg.len() + 8);
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                escaped.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                escaped.push('"');
                backslashes = 0;
            }
            other => {
                escaped.extend(std::iter::repeat_n('\\', backslashes));
                escaped.push(other);
                backslashes = 0;
            }
        }
    }
    // Double up trailing backslashes so the closing quote is not escaped away.
    escaped.extend(std::iter::repeat_n('\\', backslashes * 2));
    // Quote the whole argument so embedded whitespace is preserved.
    let quoted = format!("\"{escaped}\"");
    // Escape cmd.exe metacharacters so cmd passes them through verbatim.
    let mut out = String::with_capacity(quoted.len() + 8);
    for ch in quoted.chars() {
        if is_windows_shell_meta_char(ch) {
            out.push('^');
        }
        out.push(ch);
    }
    out
}

fn is_windows_shell_meta_char(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')'
            | ']'
            | '['
            | '%'
            | '!'
            | '^'
            | '"'
            | '`'
            | '<'
            | '>'
            | '&'
            | '|'
            | ';'
            | ','
            | ' '
            | '*'
            | '?'
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSpawnCommand {
    pub command: String,
    pub args: Vec<String>,
    pub shell: bool,
}

/// `SpawnExecutableResolution`: the function that finds an executable for a
/// spawn. Upstream's `Context.Reference` default is
/// [`resolve_spawn_executable_with_node`].
pub type SpawnExecutableFn<'a> = &'a dyn Fn(&str, Platform, &ShellEnv) -> Option<String>;

/// A spawn resolver plus the identity that keys its cache entries. Upstream
/// keys by resolver function identity (WeakMap); callers pass a distinct id
/// per resolver.
#[derive(Clone, Copy)]
pub struct SpawnResolver<'a> {
    pub id: u64,
    pub resolve: SpawnExecutableFn<'a>,
}

/// Command lookup context: the host platform, the host environment, the
/// shared resolution cache, and the monotonic clock used for cache TTLs.
#[derive(Clone, Copy)]
pub struct ShellContext<'a> {
    pub platform: Platform,
    pub host_env: &'a ShellEnv,
    pub cache: &'a CommandResolutionCache,
    pub now: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct CommandAvailabilityOptions {
    pub env: Option<ShellEnv>,
    pub extend_env: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WindowsEnvironmentProbeOptions {
    pub load_profile: bool,
}

fn trim_non_empty(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Upstream reads `NodeOS.userInfo().shell` (passwd entry). Without a libc
/// dependency this reads `$SHELL`, which login sessions set from that entry.
pub fn read_user_login_shell() -> Option<String> {
    trim_non_empty(std::env::var("SHELL").ok().as_deref())
}

/// The shells to probe for a login environment: the configured shell, the
/// user's login shell, then the platform fallback. Duplicates are dropped.
pub fn list_login_shell_candidates(
    platform: Platform,
    shell: Option<&str>,
    user_shell: Option<&str>,
) -> Vec<String> {
    let fallback_shell = match platform {
        Platform::Darwin => Some("/bin/zsh".to_owned()),
        Platform::Linux => Some("/bin/bash".to_owned()),
        _ => None,
    };
    let mut candidates: Vec<String> = Vec::new();
    for candidate in [
        trim_non_empty(shell),
        trim_non_empty(user_shell),
        fallback_shell,
    ]
    .into_iter()
    .flatten()
    {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// `launchctl getenv PATH`. Upstream's GUI-launched apps inherit only the
/// launchd PATH, so this is one of the sources the PATH merge reads.
pub fn read_path_from_launchctl(exec_file: ExecFileSync<'_>) -> Option<String> {
    trim_non_empty(
        exec_file(
            "/bin/launchctl",
            &["getenv", "PATH"],
            Duration::from_millis(2000),
        )
        .ok()
        .as_deref(),
    )
}

/// Preferred entries first, then inherited extras. Entries are trimmed and
/// deduplicated exactly (no quote stripping, no case folding).
pub fn merge_path_entries(
    preferred_path: Option<&str>,
    inherited_path: Option<&str>,
    platform: Platform,
) -> Option<String> {
    let delimiter = delimiter_for(platform);
    let mut merged: Vec<&str> = Vec::new();
    for path_value in [preferred_path, inherited_path].into_iter().flatten() {
        if path_value.is_empty() {
            continue;
        }
        for entry in path_value.split(delimiter) {
            let trimmed_entry = js_trim(entry);
            if trimmed_entry.is_empty() || merged.contains(&trimmed_entry) {
                continue;
            }
            merged.push(trimmed_entry);
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged.join(delimiter_str(platform)))
    }
}

fn env_capture_start(name: &str) -> String {
    format!("__T3CODE_ENV_{name}_START__")
}

fn env_capture_end(name: &str) -> String {
    format!("__T3CODE_ENV_{name}_END__")
}

fn is_valid_env_name(name: &str) -> bool {
    // ^[A-Z0-9_]+$
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn invalid_env_name(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Unsupported environment variable name: {name}"),
    )
}

fn build_environment_capture_command(names: &[&str]) -> io::Result<String> {
    let mut parts = Vec::with_capacity(names.len());
    for name in names {
        if !is_valid_env_name(name) {
            return Err(invalid_env_name(name));
        }
        parts.push(
            [
                format!("printf '%s\\n' '{}'", env_capture_start(name)),
                format!("printenv {name} || true"),
                format!("printf '%s\\n' '{}'", env_capture_end(name)),
            ]
            .join("; "),
        );
    }
    Ok(parts.join("; "))
}

fn build_windows_environment_capture_command(names: &[&str]) -> io::Result<String> {
    let mut parts = vec!["$ErrorActionPreference = 'Stop'".to_owned()];
    for name in names {
        if !is_valid_env_name(name) {
            return Err(invalid_env_name(name));
        }
        parts.push(format!("Write-Output '{}'", env_capture_start(name)));
        parts.push(format!(
            "$value = [Environment]::GetEnvironmentVariable('{name}')"
        ));
        parts.push(
            "if ($null -ne $value -and $value.Length -gt 0) { Write-Output $value }".to_owned(),
        );
        parts.push(format!("Write-Output '{}'", env_capture_end(name)));
    }
    Ok(parts.join("; "))
}

fn extract_environment_value(output: &str, name: &str) -> Option<String> {
    let start_marker = env_capture_start(name);
    let end_marker = env_capture_end(name);
    let start_index = output.find(&start_marker)?;
    let value_start_index = start_index + start_marker.len();
    let end_index = value_start_index + output[value_start_index..].find(&end_marker)?;

    let raw = &output[value_start_index..end_index];
    // `.replace(/^\r?\n/, "")` then `.replace(/\r?\n$/, "")`: one leading and
    // one trailing newline, nothing more.
    let value = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))
        .unwrap_or(raw);
    let value = value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value);
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn collect_environment(output: &str, names: &[&str]) -> IndexMap<String, String> {
    let mut environment = IndexMap::new();
    for name in names {
        if let Some(value) = extract_environment_value(output, name) {
            environment.insert((*name).to_owned(), value);
        }
    }
    environment
}

/// Reads named variables from `shell -ilc`, so profile-defined values come
/// through. Missing or empty variables are omitted.
pub fn read_environment_from_login_shell(
    shell: &str,
    names: &[&str],
    exec_file: ExecFileSync<'_>,
) -> io::Result<IndexMap<String, String>> {
    if names.is_empty() {
        return Ok(IndexMap::new());
    }
    let command = build_environment_capture_command(names)?;
    let output = exec_file(shell, &["-ilc", &command], Duration::from_millis(5000))?;
    Ok(collect_environment(&output, names))
}

/// Reads named variables through PowerShell (`pwsh.exe`, then
/// `powershell.exe`). Returns an empty map when neither runs.
pub fn read_environment_from_windows_shell(
    names: &[&str],
    options: &WindowsEnvironmentProbeOptions,
    exec_file: ExecFileSync<'_>,
) -> io::Result<IndexMap<String, String>> {
    if names.is_empty() {
        return Ok(IndexMap::new());
    }
    let command = build_windows_environment_capture_command(names)?;
    let mut args = vec!["-NoLogo"];
    if !options.load_profile {
        args.push("-NoProfile");
    }
    args.extend(["-NonInteractive", "-Command", command.as_str()]);
    for shell in WINDOWS_SHELL_CANDIDATES {
        match exec_file(shell, &args, Duration::from_millis(5000)) {
            Ok(output) => return Ok(collect_environment(&output, names)),
            Err(_) => continue,
        }
    }
    Ok(IndexMap::new())
}

fn strip_wrapping_quotes(value: &str) -> &str {
    value.trim_matches('"')
}

fn delimiter_for(platform: Platform) -> char {
    if platform.is_win32() {
        WINDOWS_PATH_DELIMITER
    } else {
        POSIX_PATH_DELIMITER
    }
}

fn delimiter_str(platform: Platform) -> &'static str {
    if platform.is_win32() { ";" } else { ":" }
}

fn normalize_path_entry_for_comparison(entry: &str, platform: Platform) -> String {
    let normalized = strip_wrapping_quotes(js_trim(entry));
    if platform.is_win32() {
        // JS `toLowerCase`; the Windows PATH is compared case-insensitively.
        normalized.to_lowercase()
    } else {
        normalized.to_owned()
    }
}

fn sanitize_path_entry(entry: &str, platform: Platform) -> String {
    if platform.is_win32() {
        entry.replace('"', "")
    } else {
        entry.to_owned()
    }
}

/// Merges PATH values for the spawned environment. Windows entries are
/// sanitized (stray quotes removed) and compared case-insensitively; POSIX
/// entries are compared exactly.
pub fn merge_path_values(
    preferred_path: Option<&str>,
    inherited_path: Option<&str>,
    platform: Platform,
) -> Option<String> {
    let delimiter = delimiter_for(platform);
    let mut merged: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for raw_value in [preferred_path, inherited_path].into_iter().flatten() {
        if raw_value.is_empty() {
            continue;
        }
        for entry in raw_value.split(delimiter) {
            let sanitized = sanitize_path_entry(js_trim(entry), platform);
            if sanitized.is_empty() {
                continue;
            }
            let normalized = normalize_path_entry_for_comparison(&sanitized, platform);
            if normalized.is_empty() || seen.contains(&normalized) {
                continue;
            }
            seen.insert(normalized);
            merged.push(sanitized);
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged.join(delimiter_str(platform)))
    }
}

/// `PATH` from the environment, checking the spellings Windows uses.
fn read_env_path(env: &ShellEnv) -> Option<&str> {
    env.get("PATH")
        .or_else(|| env.get("Path"))
        .or_else(|| env.get("path"))
        .map(String::as_str)
}

fn resolve_path_environment_variable(env: &ShellEnv) -> &str {
    read_env_path(env).unwrap_or("")
}

/// Known Windows CLI install directories, in priority order. Used only as a
/// fallback behind the shell's own PATH.
pub fn resolve_known_windows_cli_dirs(env: &ShellEnv) -> Vec<String> {
    let app_data = env
        .get("APPDATA")
        .map(|value| js_trim(value))
        .filter(|value| !value.is_empty());
    let local_app_data = env
        .get("LOCALAPPDATA")
        .map(|value| js_trim(value))
        .filter(|value| !value.is_empty());
    let user_profile = env
        .get("USERPROFILE")
        .map(|value| js_trim(value))
        .filter(|value| !value.is_empty());

    let mut dirs = Vec::new();
    if let Some(app_data) = app_data {
        dirs.push(format!("{app_data}\\npm"));
    }
    if let Some(local) = local_app_data {
        dirs.push(format!("{local}\\Programs\\nodejs"));
        dirs.push(format!("{local}\\Volta\\bin"));
    }
    if let Some(local) = local_app_data {
        dirs.push(format!("{local}\\pnpm"));
    }
    if let Some(profile) = user_profile {
        dirs.push(format!("{profile}\\.local\\bin"));
        dirs.push(format!("{profile}\\.bun\\bin"));
        dirs.push(format!("{profile}\\scoop\\shims"));
    }
    dirs
}

/// `path.win32.extname` / `path.posix.extname`: the last `.` of the basename,
/// unless it is the first character.
fn extname(path: &str, platform: Platform) -> &str {
    let base = basename(path, platform);
    match base.rfind('.') {
        Some(index) if index > 0 => &base[index..],
        _ => "",
    }
}

fn basename(path: &str, platform: Platform) -> &str {
    let trimmed = path.trim_end_matches(|ch| separator_char(ch, platform));
    let start = trimmed
        .rfind(|ch| separator_char(ch, platform))
        .map(|index| index + 1)
        .unwrap_or(0);
    &trimmed[start..]
}

fn separator_char(ch: char, platform: Platform) -> bool {
    if platform.is_win32() {
        ch == '\\' || ch == '/'
    } else {
        ch == '/'
    }
}

/// `path.join(dir, name)` for one directory and one file name.
fn join_path(directory: &str, name: &str, platform: Platform) -> String {
    let separator = if platform.is_win32() { '\\' } else { '/' };
    let trimmed = directory.trim_end_matches(|ch| separator_char(ch, platform));
    if trimmed.is_empty() {
        // A bare root ("/" or "\") keeps its separator.
        format!("{separator}{name}")
    } else {
        format!("{trimmed}{separator}{name}")
    }
}

/// `resolveCommandCandidates`: POSIX gets the bare name. Windows gets every
/// PATHEXT extension in upper and lower case, or only the ones that exist
/// already when the command names a PATHEXT extension.
fn resolve_command_candidates(
    command: &str,
    platform: Platform,
    windows_path_extensions: &[String],
) -> Vec<String> {
    if !platform.is_win32() {
        return vec![command.to_owned()];
    }
    let extension = extname(command, platform);
    let normalized_extension = extension.to_uppercase();

    if !extension.is_empty() && windows_path_extensions.contains(&normalized_extension) {
        let without_extension = &command[..command.len() - extension.len()];
        let mut candidates = vec![
            command.to_owned(),
            format!("{without_extension}{normalized_extension}"),
            format!("{without_extension}{}", normalized_extension.to_lowercase()),
        ];
        dedupe_in_place(&mut candidates);
        return candidates;
    }

    let mut candidates = Vec::new();
    for candidate_extension in windows_path_extensions {
        candidates.push(format!("{command}{candidate_extension}"));
        candidates.push(format!("{command}{}", candidate_extension.to_lowercase()));
    }
    dedupe_in_place(&mut candidates);
    candidates
}

fn dedupe_in_place(values: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

/// `PATHEXT` as uppercase `.EXT` entries; the Windows default when unset.
fn resolve_windows_path_extensions(env: &ShellEnv) -> Vec<String> {
    let fallback = vec![
        ".COM".to_owned(),
        ".EXE".to_owned(),
        ".BAT".to_owned(),
        ".CMD".to_owned(),
    ];
    let Some(raw_value) = env.get("PATHEXT") else {
        return fallback;
    };
    if raw_value.is_empty() {
        return fallback;
    }
    let mut parsed: Vec<String> = Vec::new();
    for entry in raw_value.split(';') {
        let trimmed = js_trim(entry);
        if trimmed.is_empty() {
            continue;
        }
        let normalized = if trimmed.starts_with('.') {
            trimmed.to_uppercase()
        } else {
            format!(".{}", trimmed.to_uppercase())
        };
        if !parsed.contains(&normalized) {
            parsed.push(normalized);
        }
    }
    if parsed.is_empty() { fallback } else { parsed }
}

fn is_file(path: &str) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

/// Approximates `access(path, X_OK)` with the execute bits (no uid check).
fn can_execute_file(path: &str) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Node-style spawn resolver. The default for [`SpawnResolver`].
pub fn resolve_spawn_executable_with_node(
    command: &str,
    platform: Platform,
    env: &ShellEnv,
) -> Option<String> {
    let windows_path_extensions = if platform.is_win32() {
        resolve_windows_path_extensions(env)
    } else {
        Vec::new()
    };
    let candidates = resolve_command_candidates(command, platform, &windows_path_extensions);
    let is_executable = |candidate: &str| {
        if !is_file(candidate) {
            return false;
        }
        if platform.is_win32() {
            return windows_path_extensions.contains(&extname(candidate, platform).to_uppercase());
        }
        can_execute_file(candidate)
    };

    if command.contains('/') || command.contains('\\') {
        return candidates
            .into_iter()
            .find(|candidate| is_executable(candidate));
    }

    for path_entry in read_env_path(env)
        .unwrap_or("")
        .split(delimiter_for(platform))
    {
        let normalized_path_entry = strip_wrapping_quotes(js_trim(path_entry));
        if normalized_path_entry.is_empty() {
            continue;
        }
        for candidate in &candidates {
            let candidate_path = join_path(normalized_path_entry, candidate, platform);
            if is_executable(&candidate_path) {
                return Some(candidate_path);
            }
        }
    }
    None
}

// Session bootstrap resolves the same commands over and over, each PATH scan
// costing hundreds of filesystem probes. Memoize the scan outcome per
// (platform, PATH, PATHEXT, command) for a short window: repeat scans hit the
// cache while any change to the search environment invalidates immediately.
// Explicit-path resolution is never cached - callers probe paths they have
// just written. A "not-found" outcome is also cached for the TTL.
const COMMAND_RESOLUTION_CACHE_TTL: Duration = Duration::from_secs(30);
const COMMAND_RESOLUTION_CACHE_MAX_ENTRIES: usize = 512;
const COMMAND_RESOLUTION_CACHE_KEY_SEPARATOR: &str = "\0";

#[derive(Debug, Clone)]
struct CommandResolutionCacheEntry {
    resolved_path: Option<String>,
    expires_at: Instant,
}

/// The `CommandResolutionCache` Map. Entries are evicted oldest-first, as a JS
/// `Map` iterates in insertion order.
#[derive(Default)]
pub struct CommandResolutionCache {
    entries: Mutex<IndexMap<String, CommandResolutionCacheEntry>>,
}

impl CommandResolutionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process-wide cache shared by every consumer that does not inject one.
    pub fn global() -> &'static CommandResolutionCache {
        static GLOBAL: OnceLock<CommandResolutionCache> = OnceLock::new();
        GLOBAL.get_or_init(CommandResolutionCache::new)
    }

    /// The unexpired entry for `key`, if any. `Some(None)` is a cached miss.
    fn fresh(&self, key: &str, now: Instant) -> Option<Option<String>> {
        let entries = self.entries.lock();
        let entry = entries.get(key)?;
        (entry.expires_at > now).then(|| entry.resolved_path.clone())
    }

    fn store(&self, key: String, resolved_path: Option<String>, now: Instant) {
        let mut entries = self.entries.lock();
        if entries.len() >= COMMAND_RESOLUTION_CACHE_MAX_ENTRIES {
            entries.shift_remove_index(0);
        }
        entries.insert(
            key,
            CommandResolutionCacheEntry {
                resolved_path,
                expires_at: now + COMMAND_RESOLUTION_CACHE_TTL,
            },
        );
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().len()
    }
}

fn is_executable_file(
    file_path: &str,
    platform: Platform,
    windows_path_extensions: &[String],
) -> bool {
    if !is_file(file_path) {
        return false;
    }
    if platform.is_win32() {
        let extension = extname(file_path, platform);
        if extension.is_empty() {
            return false;
        }
        return windows_path_extensions.contains(&extension.to_uppercase());
    }
    can_execute_file(file_path)
}

/// `resolveCommandPathForPlatform`. Walks PATH once per (platform, PATH, PATHEXT,
/// command) and caches the answer, including misses.
fn resolve_command_path_for_platform(
    command: &str,
    env: &ShellEnv,
    ctx: &ShellContext<'_>,
) -> Option<String> {
    let platform = ctx.platform;
    let windows_path_extensions = if platform.is_win32() {
        resolve_windows_path_extensions(env)
    } else {
        Vec::new()
    };
    let command_candidates =
        resolve_command_candidates(command, platform, &windows_path_extensions);

    if command.contains('/') || command.contains('\\') {
        return command_candidates
            .into_iter()
            .find(|candidate| is_executable_file(candidate, platform, &windows_path_extensions));
    }

    let path_value = resolve_path_environment_variable(env);
    if path_value.is_empty() {
        return None;
    }

    let cache_key = [
        platform_key(platform),
        path_value,
        windows_path_extensions.join(";").as_str(),
        command,
    ]
    .join(COMMAND_RESOLUTION_CACHE_KEY_SEPARATOR);
    if let Some(cached) = ctx.cache.fresh(&cache_key, ctx.now) {
        return cached;
    }

    // Keep case variants: Windows can make PATH directories case-sensitive.
    let mut path_entries: Vec<&str> = Vec::new();
    for entry in path_value.split(delimiter_for(platform)) {
        let path_entry = strip_wrapping_quotes(js_trim(entry));
        if path_entry.is_empty() || path_entries.contains(&path_entry) {
            continue;
        }
        path_entries.push(path_entry);
    }

    for path_entry in path_entries {
        for candidate in &command_candidates {
            // Upstream joins with the host `Path` service here, not the target
            // platform's, so the probe reaches the file the host can open.
            let candidate_path = Path::new(path_entry)
                .join(candidate)
                .to_string_lossy()
                .into_owned();
            if is_executable_file(&candidate_path, platform, &windows_path_extensions) {
                ctx.cache
                    .store(cache_key, Some(candidate_path.clone()), ctx.now);
                return Some(candidate_path);
            }
        }
    }
    ctx.cache.store(cache_key, None, ctx.now);
    None
}

fn platform_key(platform: Platform) -> &'static str {
    match platform {
        Platform::Win32 => "win32",
        Platform::Darwin => "darwin",
        Platform::Linux => "linux",
        Platform::Other => "other",
    }
}

/// `resolveCommandPath`: the absolute path of `command`, or `None`.
pub fn resolve_command_path(
    command: &str,
    options: &CommandAvailabilityOptions,
    ctx: &ShellContext<'_>,
) -> Option<String> {
    let env = options.env.as_ref().unwrap_or(ctx.host_env);
    resolve_command_path_for_platform(command, env, ctx)
}

/// `isCommandAvailable`.
pub fn is_command_available(
    command: &str,
    options: &CommandAvailabilityOptions,
    ctx: &ShellContext<'_>,
) -> bool {
    resolve_command_path(command, options, ctx).is_some()
}

// Git for Windows 2.56 moved x64 builds from mingw64 to ucrt64; ARM64 builds
// live in clangarm64 and 32-bit ones in mingw32.
const GIT_FOR_WINDOWS_BUILDS: [&str; 4] = ["ucrt64", "clangarm64", "mingw64", "mingw32"];

/// Windows-path `dirname` for the Git launcher swap. Only the last separator
/// is consulted; a bare name yields ".".
fn win32_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    match trimmed.rfind(['\\', '/']) {
        Some(0) => trimmed[..1].to_owned(),
        Some(index) => trimmed[..index].to_owned(),
        None => ".".to_owned(),
    }
}

/// Swaps Git for Windows' launcher (`<Git>\cmd\git.exe`, or the portable
/// build's `<Git>\bin\git.exe`) for the git.exe it starts. The launcher costs a
/// second process on every git command. The launcher stays when MSYSTEM is set.
pub fn prefer_git_for_windows_binary(
    executable: &str,
    env: &ShellEnv,
    is_file: &dyn Fn(&str) -> bool,
) -> String {
    if env.get("MSYSTEM").is_some_and(|value| !value.is_empty()) {
        return executable.to_owned();
    }
    if basename(executable, Platform::Win32).to_lowercase() != "git.exe" {
        return executable.to_owned();
    }
    let launcher_directory = win32_dirname(executable);
    let launcher_folder = basename(&launcher_directory, Platform::Win32).to_lowercase();
    if launcher_folder != "cmd" && launcher_folder != "bin" {
        return executable.to_owned();
    }
    let install_root = win32_dirname(&launcher_directory);
    for build in GIT_FOR_WINDOWS_BUILDS {
        let candidate = format!("{install_root}\\{build}\\bin\\git.exe");
        if is_file(&candidate) {
            return candidate;
        }
    }
    executable.to_owned()
}

/// Resolves `command` for a spawn. On POSIX the command runs as-is. On Windows
/// the executable is resolved, `.cmd`/`.bat` shims become one escaped
/// `cmd.exe` line with no args, and unresolved commands fall through unchanged.
pub fn resolve_spawn_command(
    command: &str,
    args: &[&str],
    options: &CommandAvailabilityOptions,
    resolver: SpawnResolver<'_>,
    ctx: &ShellContext<'_>,
) -> ResolvedSpawnCommand {
    let platform = ctx.platform;
    if !platform.is_win32() {
        return ResolvedSpawnCommand {
            command: command.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            shell: false,
        };
    }

    let env: ShellEnv = match &options.env {
        None => ctx.host_env.clone(),
        Some(env) if options.extend_env => {
            let mut merged = ctx.host_env.clone();
            for (key, value) in env {
                merged.insert(key.clone(), value.clone());
            }
            merged
        }
        Some(env) => env.clone(),
    };

    // The scan is synchronous and runs before every child process, so it shares
    // the cache above. Explicit paths stay uncached, and so do misses: a failed
    // spawn is how providers report "not installed", and that has to clear the
    // moment the binary appears.
    let explicit_path = command.contains('/') || command.contains('\\');
    let resolver_key = resolver.id.to_string();
    let path_extensions_key = resolve_windows_path_extensions(&env).join(";");
    let msystem_key = if env.get("MSYSTEM").is_some_and(|value| !value.is_empty()) {
        "msystem"
    } else {
        ""
    };
    let cache_key = [
        "spawn",
        resolver_key.as_str(),
        platform_key(platform),
        resolve_path_environment_variable(&env),
        path_extensions_key.as_str(),
        msystem_key,
        command,
    ]
    .join(COMMAND_RESOLUTION_CACHE_KEY_SEPARATOR);

    let cached = if explicit_path {
        None
    } else {
        ctx.cache.fresh(&cache_key, ctx.now)
    };
    let resolved_executable = match cached {
        Some(resolved) => resolved,
        None => {
            // Cached with the scan: its file checks would otherwise run before
            // every git launch.
            let found = (resolver.resolve)(command, platform, &env);
            let resolved = found.map(|found| {
                prefer_git_for_windows_binary(&found, &env, &|candidate| is_file(candidate))
            });
            if !explicit_path && let Some(resolved) = &resolved {
                ctx.cache.store(cache_key, Some(resolved.clone()), ctx.now);
            }
            resolved
        }
    };

    let resolved_command = resolved_executable.unwrap_or_else(|| command.to_owned());
    let extension = extname(&resolved_command, Platform::Win32).to_lowercase();
    if extension != ".cmd" && extension != ".bat" {
        return ResolvedSpawnCommand {
            command: resolved_command,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            shell: false,
        };
    }

    // Node runs a shell spawn as `cmd.exe /d /s /c "<command> <args...>"`, but
    // passing args with `shell: true` prints DEP0190. Build the same escaped line
    // here and pass no args.
    let mut line_parts = vec![resolved_command];
    line_parts.extend(args.iter().map(|arg| (*arg).to_owned()));
    ResolvedSpawnCommand {
        command: line_parts
            .iter()
            .map(|part| escape_windows_shell_arg(part))
            .collect::<Vec<_>>()
            .join(" "),
        args: Vec::new(),
        shell: true,
    }
}

/// `WindowsShellEnvironment`: reads the named variables from a Windows shell.
pub type WindowsEnvironmentReader<'a> =
    &'a dyn Fn(&[&str], &WindowsEnvironmentProbeOptions) -> IndexMap<String, String>;

/// `resolveKnownWindowsCliDirs` plus the fallback probe: the shell's PATH
/// first, then the known CLI directories behind it, then (if `node` still does
/// not resolve) the profiled PowerShell PATH and fnm variables.
pub fn resolve_windows_environment(
    env: &ShellEnv,
    read_environment: WindowsEnvironmentReader<'_>,
    command_available: &dyn Fn(&str, &ShellEnv) -> bool,
) -> ShellEnv {
    let inherited_path = read_env_path(env);
    let shell_path = read_environment(
        &["PATH"],
        &WindowsEnvironmentProbeOptions {
            load_profile: false,
        },
    )
    .get("PATH")
    .cloned();
    let merged_path = merge_path_values(shell_path.as_deref(), inherited_path, Platform::Win32);
    let known_cli_path = resolve_known_windows_cli_dirs(env).join(";");
    // Preserve the order a user's shell uses. These directories fill gaps when
    // desktop apps launch without the full interactive-shell PATH.
    let baseline_path = merge_path_values(
        merged_path.as_deref(),
        Some(&known_cli_path),
        Platform::Win32,
    );
    let mut baseline_patch = ShellEnv::new();
    if let Some(path) = &baseline_path {
        baseline_patch.insert("PATH".to_owned(), path.clone());
    }
    let baseline_env = merge_env(env, &baseline_patch);

    if command_available("node", &baseline_env) {
        return baseline_patch;
    }

    let profiled = read_environment(
        &["PATH", "FNM_DIR", "FNM_MULTISHELL_PATH"],
        &WindowsEnvironmentProbeOptions { load_profile: true },
    );
    let profiled_path = merge_path_values(
        profiled.get("PATH").map(String::as_str),
        baseline_path.as_deref(),
        Platform::Win32,
    );
    let mut profiled_patch = ShellEnv::new();
    if let Some(path) = profiled_path {
        profiled_patch.insert("PATH".to_owned(), path);
    }
    for key in ["FNM_DIR", "FNM_MULTISHELL_PATH"] {
        if let Some(value) = profiled.get(key) {
            profiled_patch.insert(key.to_owned(), value.clone());
        }
    }
    if profiled_patch.is_empty() {
        baseline_patch
    } else {
        let mut merged = baseline_patch;
        merged.extend(profiled_patch);
        merged
    }
}

fn merge_env(current: &ShellEnv, patch: &ShellEnv) -> ShellEnv {
    let mut next = current.clone();
    for (key, value) in patch {
        next.insert(key.clone(), value.clone());
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> ShellEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn ctx<'a>(
        platform: Platform,
        host_env: &'a ShellEnv,
        cache: &'a CommandResolutionCache,
        now: Instant,
    ) -> ShellContext<'a> {
        ShellContext {
            platform,
            host_env,
            cache,
            now,
        }
    }

    fn no_resolver() -> impl Fn(&str, Platform, &ShellEnv) -> Option<String> {
        |_, _, _| None
    }

    fn fail_exec(_: &str, _: &[&str], _: Duration) -> io::Result<String> {
        Err(io::Error::other("unexpected exec"))
    }

    #[test]
    fn reads_environment_from_login_shell_with_printenv_probe() {
        let calls = std::cell::RefCell::new(Vec::new());
        let exec = |file: &str, args: &[&str], timeout: Duration| {
            calls.borrow_mut().push((
                file.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
                timeout,
            ));
            Ok("__T3CODE_ENV_PATH_START__\n/a:/b\n__T3CODE_ENV_PATH_END__\n".to_owned())
        };
        let result =
            read_environment_from_login_shell("/opt/homebrew/bin/fish", &["PATH"], &exec).unwrap();
        assert_eq!(result.get("PATH").map(String::as_str), Some("/a:/b"));
        let calls = calls.borrow();
        assert_eq!(calls.len(), 1);
        let (shell, args, timeout) = &calls[0];
        assert_eq!(shell, "/opt/homebrew/bin/fish");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-ilc");
        assert!(args[1].contains("printenv PATH || true"));
        assert!(args[1].contains("__T3CODE_ENV_PATH_START__"));
        assert!(args[1].contains("__T3CODE_ENV_PATH_END__"));
        assert_eq!(*timeout, Duration::from_millis(5000));
    }

    #[test]
    fn reads_path_from_launchctl_trimmed() {
        let calls = std::cell::RefCell::new(Vec::new());
        let exec = |file: &str, args: &[&str], timeout: Duration| {
            calls.borrow_mut().push((
                file.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
                timeout,
            ));
            Ok("  /opt/homebrew/bin:/usr/bin  \n".to_owned())
        };
        assert_eq!(
            read_path_from_launchctl(&exec).as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(
            calls.borrow()[0],
            (
                "/bin/launchctl".to_owned(),
                vec!["getenv".to_owned(), "PATH".to_owned()],
                Duration::from_millis(2000)
            )
        );
    }

    #[test]
    fn launchctl_is_none_when_unavailable() {
        let exec = |_: &str, _: &[&str], _: Duration| -> io::Result<String> {
            Err(io::Error::other("spawn /bin/launchctl ENOENT"))
        };
        assert_eq!(read_path_from_launchctl(&exec), None);
    }

    #[test]
    fn extracts_multiple_variables_from_login_shell_output() {
        let exec = |_: &str, _: &[&str], _: Duration| {
            Ok([
                "__T3CODE_ENV_PATH_START__",
                "/a:/b",
                "__T3CODE_ENV_PATH_END__",
                "__T3CODE_ENV_SSH_AUTH_SOCK_START__",
                "/tmp/secretive.sock",
                "__T3CODE_ENV_SSH_AUTH_SOCK_END__",
            ]
            .join("\n"))
        };
        let result =
            read_environment_from_login_shell("/bin/zsh", &["PATH", "SSH_AUTH_SOCK"], &exec)
                .unwrap();
        assert_eq!(
            result,
            env(&[("PATH", "/a:/b"), ("SSH_AUTH_SOCK", "/tmp/secretive.sock")])
        );
    }

    #[test]
    fn omits_missing_or_empty_login_shell_variables() {
        let exec = |_: &str, _: &[&str], _: Duration| {
            Ok([
                "__T3CODE_ENV_PATH_START__",
                "/a:/b",
                "__T3CODE_ENV_PATH_END__",
                "__T3CODE_ENV_SSH_AUTH_SOCK_START__",
                "__T3CODE_ENV_SSH_AUTH_SOCK_END__",
            ]
            .join("\n"))
        };
        let result =
            read_environment_from_login_shell("/bin/zsh", &["PATH", "SSH_AUTH_SOCK"], &exec)
                .unwrap();
        assert_eq!(result, env(&[("PATH", "/a:/b")]));
    }

    #[test]
    fn preserves_surrounding_whitespace_in_captured_values() {
        let exec = |_: &str, _: &[&str], _: Duration| {
            Ok([
                "__T3CODE_ENV_CUSTOM_VAR_START__",
                "  padded value  ",
                "__T3CODE_ENV_CUSTOM_VAR_END__",
            ]
            .join("\n"))
        };
        let result = read_environment_from_login_shell("/bin/zsh", &["CUSTOM_VAR"], &exec).unwrap();
        assert_eq!(result, env(&[("CUSTOM_VAR", "  padded value  ")]));
    }

    #[test]
    fn rejects_environment_names_outside_the_capture_pattern() {
        let error =
            read_environment_from_login_shell("/bin/zsh", &["bad-name"], &fail_exec).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn login_shell_candidates_put_env_shell_first_without_duplicates() {
        assert_eq!(
            list_login_shell_candidates(
                Platform::Darwin,
                Some(" /opt/homebrew/bin/nu "),
                Some("/bin/zsh")
            ),
            vec!["/opt/homebrew/bin/nu".to_owned(), "/bin/zsh".to_owned()]
        );
        assert_eq!(
            list_login_shell_candidates(Platform::Linux, None, Some("")),
            vec!["/bin/bash".to_owned()]
        );
        assert_eq!(
            list_login_shell_candidates(Platform::Linux, Some("/bin/bash"), Some("/bin/bash")),
            vec!["/bin/bash".to_owned()]
        );
    }

    #[test]
    fn merge_path_entries_prefers_login_shell_and_keeps_inherited_extras() {
        assert_eq!(
            merge_path_entries(
                Some("/opt/homebrew/bin:/usr/bin"),
                Some("/Users/test/.local/bin:/usr/bin"),
                Platform::Darwin
            )
            .as_deref(),
            Some("/opt/homebrew/bin:/usr/bin:/Users/test/.local/bin")
        );
        assert_eq!(
            merge_path_entries(
                Some("C:\\Tools;C:\\Windows"),
                Some("C:\\Windows;C:\\Git"),
                Platform::Win32
            )
            .as_deref(),
            Some("C:\\Tools;C:\\Windows;C:\\Git")
        );
    }

    #[test]
    fn reads_windows_environment_through_powershell() {
        let calls = std::cell::RefCell::new(Vec::new());
        let exec = |file: &str, args: &[&str], _: Duration| {
            calls.borrow_mut().push((
                file.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            ));
            Ok("__T3CODE_ENV_PATH_START__\nC:\\Users\\testuser\\AppData\\Roaming\\npm\n__T3CODE_ENV_PATH_END__\n".to_owned())
        };
        let result = read_environment_from_windows_shell(
            &["PATH"],
            &WindowsEnvironmentProbeOptions::default(),
            &exec,
        )
        .unwrap();
        assert_eq!(
            result,
            env(&[("PATH", "C:\\Users\\testuser\\AppData\\Roaming\\npm")])
        );
        let calls = calls.borrow();
        assert_eq!(calls[0].0, "pwsh.exe");
        for flag in ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"] {
            assert!(calls[0].1.iter().any(|arg| arg == flag), "missing {flag}");
        }
    }

    #[test]
    fn strips_crlf_delimiters_from_powershell_values() {
        let exec = |_: &str, _: &[&str], _: Duration| {
            Ok("__T3CODE_ENV_FNM_DIR_START__\r\nC:\\Users\\testuser\\AppData\\Roaming\\fnm\r\n__T3CODE_ENV_FNM_DIR_END__\r\n".to_owned())
        };
        let result = read_environment_from_windows_shell(
            &["FNM_DIR"],
            &WindowsEnvironmentProbeOptions::default(),
            &exec,
        )
        .unwrap();
        assert_eq!(
            result,
            env(&[("FNM_DIR", "C:\\Users\\testuser\\AppData\\Roaming\\fnm")])
        );
    }

    #[test]
    fn omits_no_profile_when_load_profile_is_enabled() {
        let calls = std::cell::RefCell::new(Vec::new());
        let exec = |_: &str, args: &[&str], _: Duration| {
            calls
                .borrow_mut()
                .push(args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
            Ok("__T3CODE_ENV_PATH_START__\nC:\\Tools\n__T3CODE_ENV_PATH_END__\n".to_owned())
        };
        let result = read_environment_from_windows_shell(
            &["PATH"],
            &WindowsEnvironmentProbeOptions { load_profile: true },
            &exec,
        )
        .unwrap();
        assert_eq!(result, env(&[("PATH", "C:\\Tools")]));
        let args = &calls.borrow()[0];
        assert!(args.iter().any(|arg| arg == "-NonInteractive"));
        assert!(!args.iter().any(|arg| arg == "-NoProfile"));
    }

    #[test]
    fn falls_back_to_windows_powershell_when_pwsh_is_missing() {
        let calls = std::cell::RefCell::new(Vec::new());
        let exec = |file: &str, _: &[&str], _: Duration| {
            calls.borrow_mut().push(file.to_owned());
            if file == "pwsh.exe" {
                return Err(io::Error::other("spawn pwsh.exe ENOENT"));
            }
            Ok("__T3CODE_ENV_PATH_START__\nC:\\Tools\n__T3CODE_ENV_PATH_END__\n".to_owned())
        };
        let result = read_environment_from_windows_shell(
            &["PATH"],
            &WindowsEnvironmentProbeOptions::default(),
            &exec,
        )
        .unwrap();
        assert_eq!(result, env(&[("PATH", "C:\\Tools")]));
        assert_eq!(
            *calls.borrow(),
            vec!["pwsh.exe".to_owned(), "powershell.exe".to_owned()]
        );
    }

    #[test]
    fn merge_path_values_sanitizes_and_dedupes_windows_entries() {
        assert_eq!(
            merge_path_values(
                Some("C:\\Users\\testuser\\AppData\\Roaming\\npm;\"C:\\Program Files\\nodejs\""),
                Some("c:\\users\\testuser\\appdata\\roaming\\npm;C:\\Windows\\System32"),
                Platform::Win32,
            )
            .as_deref(),
            Some(
                "C:\\Users\\testuser\\AppData\\Roaming\\npm;C:\\Program Files\\nodejs;C:\\Windows\\System32"
            )
        );
    }

    #[test]
    fn merge_path_values_removes_stray_quotes_from_windows_entries() {
        assert_eq!(
            merge_path_values(
                Some("C:\\Windows\\System32;C:\\cloudflared.exe;C:\";C:\\Program Files\\nodejs"),
                None,
                Platform::Win32,
            )
            .as_deref(),
            Some("C:\\Windows\\System32;C:\\cloudflared.exe;C:;C:\\Program Files\\nodejs")
        );
    }

    #[test]
    fn merge_path_values_dedupes_case_sensitively_on_posix() {
        assert_eq!(
            merge_path_values(
                Some("/usr/local/bin:/usr/bin"),
                Some("/usr/bin:/USR/BIN"),
                Platform::Linux
            )
            .as_deref(),
            Some("/usr/local/bin:/usr/bin:/USR/BIN")
        );
    }

    #[test]
    fn known_windows_cli_dirs_follow_priority_order() {
        assert_eq!(
            resolve_known_windows_cli_dirs(&env(&[
                ("APPDATA", "C:\\Users\\testuser\\AppData\\Roaming"),
                ("LOCALAPPDATA", "C:\\Users\\testuser\\AppData\\Local"),
                ("USERPROFILE", "C:\\Users\\testuser"),
            ])),
            vec![
                "C:\\Users\\testuser\\AppData\\Roaming\\npm",
                "C:\\Users\\testuser\\AppData\\Local\\Programs\\nodejs",
                "C:\\Users\\testuser\\AppData\\Local\\Volta\\bin",
                "C:\\Users\\testuser\\AppData\\Local\\pnpm",
                "C:\\Users\\testuser\\.local\\bin",
                "C:\\Users\\testuser\\.bun\\bin",
                "C:\\Users\\testuser\\scoop\\shims",
            ]
        );
    }

    #[test]
    fn command_lookup_returns_none_when_missing() {
        let host = ShellEnv::new();
        let cache = CommandResolutionCache::new();
        let now = Instant::now();
        let options = CommandAvailabilityOptions {
            env: Some(env(&[("PATH", "")])),
            extend_env: false,
        };
        assert_eq!(
            resolve_command_path(
                "missing",
                &options,
                &ctx(Platform::Linux, &host, &cache, now)
            ),
            None
        );
    }

    #[test]
    fn command_lookup_is_none_when_path_is_empty() {
        let host = ShellEnv::new();
        let cache = CommandResolutionCache::new();
        let now = Instant::now();
        let options = CommandAvailabilityOptions {
            env: Some(env(&[("PATH", ""), ("PATHEXT", ".COM;.EXE;.BAT;.CMD")])),
            extend_env: false,
        };
        assert!(!is_command_available(
            "definitely-not-installed",
            &options,
            &ctx(Platform::Win32, &host, &cache, now)
        ));
    }

    #[test]
    fn command_lookup_finds_available_windows_commands() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let host = ShellEnv::new();
        let cache = CommandResolutionCache::new();
        let now = Instant::now();
        let options = CommandAvailabilityOptions {
            env: Some(env(&[("PATH", &cwd), ("PATHEXT", ".CMD")])),
            extend_env: false,
        };
        let windows_file = directory.path().join("available.cmd");
        fs::write(&windows_file, "").unwrap();
        let context = ctx(Platform::Win32, &host, &cache, now);
        // Case-insensitive hosts report the candidate's spelling, so compare folded.
        let resolved = resolve_command_path("available", &options, &context).unwrap();
        assert_eq!(
            resolved.to_lowercase(),
            windows_file.to_str().unwrap().to_lowercase()
        );
        assert_eq!(
            resolve_command_path(windows_file.to_str().unwrap(), &options, &context).as_deref(),
            windows_file.to_str()
        );
    }

    #[test]
    fn resolves_lowercase_executable_for_uppercase_pathext_command() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let executable = directory.path().join("audit-command.cmd");
        fs::write(&executable, "@echo off\n").unwrap();
        let host = ShellEnv::new();
        let cache = CommandResolutionCache::new();
        let options = CommandAvailabilityOptions {
            env: Some(env(&[("PATH", &cwd), ("PATHEXT", ".CMD")])),
            extend_env: false,
        };
        for command in ["audit-command", "audit-command.CMD"] {
            let resolved = resolve_command_path(
                command,
                &options,
                &ctx(Platform::Win32, &host, &cache, Instant::now()),
            )
            .expect("lowercase executable should resolve");
            // A case-insensitive host reports the spelling the candidate used.
            assert!(
                resolved.to_lowercase().ends_with("audit-command.cmd"),
                "{command} -> {resolved}"
            );
        }
    }

    #[test]
    fn keeps_cached_misses_until_expiry_while_allowing_explicit_paths() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().to_string_lossy().into_owned();
        let executable = directory.path().join("appeared.CMD");
        let host = ShellEnv::new();
        let cache = CommandResolutionCache::new();
        let start = Instant::now();
        let options = CommandAvailabilityOptions {
            env: Some(env(&[
                ("PATH", &format!("{cwd};{cwd}")),
                ("PATHEXT", ".CMD"),
            ])),
            extend_env: false,
        };
        let at = |offset: u64| {
            ctx(
                Platform::Win32,
                &host,
                &cache,
                start + Duration::from_secs(offset),
            )
        };

        assert_eq!(resolve_command_path("appeared", &options, &at(0)), None);
        fs::write(&executable, "@echo off\n").unwrap();
        assert_eq!(resolve_command_path("appeared", &options, &at(0)), None);
        assert_eq!(
            resolve_command_path(executable.to_str().unwrap(), &options, &at(0)).as_deref(),
            executable.to_str()
        );
        assert_eq!(
            resolve_command_path("appeared", &options, &at(30)).as_deref(),
            executable.to_str()
        );
    }

    #[test]
    fn prefers_git_for_windows_binary_beside_launcher() {
        let present = |paths: &'static [&'static str]| move |file: &str| paths.contains(&file);
        assert_eq!(
            prefer_git_for_windows_binary(
                "C:\\Program Files\\Git\\cmd\\git.EXE",
                &ShellEnv::new(),
                &present(&[
                    "C:\\Program Files\\Git\\ucrt64\\bin\\git.exe",
                    "C:\\Program Files\\Git\\mingw64\\bin\\git.exe",
                ]),
            ),
            "C:\\Program Files\\Git\\ucrt64\\bin\\git.exe"
        );
        assert_eq!(
            prefer_git_for_windows_binary(
                "D:\\PortableGit\\bin\\git.exe",
                &ShellEnv::new(),
                &present(&["D:\\PortableGit\\mingw64\\bin\\git.exe"]),
            ),
            "D:\\PortableGit\\mingw64\\bin\\git.exe"
        );
        assert_eq!(
            prefer_git_for_windows_binary(
                "C:\\Program Files\\Git\\cmd\\git.exe",
                &ShellEnv::new(),
                &present(&["C:\\Program Files\\Git\\clangarm64\\bin\\git.exe"]),
            ),
            "C:\\Program Files\\Git\\clangarm64\\bin\\git.exe"
        );
    }

    #[test]
    fn keeps_git_launcher_when_msystem_is_set() {
        assert_eq!(
            prefer_git_for_windows_binary(
                "C:\\Program Files\\Git\\cmd\\git.exe",
                &env(&[("MSYSTEM", "MINGW64")]),
                &|file| file == "C:\\Program Files\\Git\\mingw64\\bin\\git.exe",
            ),
            "C:\\Program Files\\Git\\cmd\\git.exe"
        );
    }

    #[test]
    fn keeps_git_launcher_when_no_git_exe_sits_beside_it() {
        assert_eq!(
            prefer_git_for_windows_binary(
                "C:\\Program Files\\Git\\cmd\\git.exe",
                &ShellEnv::new(),
                &|_| false
            ),
            "C:\\Program Files\\Git\\cmd\\git.exe"
        );
    }

    #[test]
    fn leaves_other_gits_and_launchers_alone() {
        let everything = |_: &str| true;
        assert_eq!(
            prefer_git_for_windows_binary(
                "C:\\Users\\me\\scoop\\shims\\git.exe",
                &ShellEnv::new(),
                &everything
            ),
            "C:\\Users\\me\\scoop\\shims\\git.exe"
        );
        assert_eq!(
            prefer_git_for_windows_binary(
                "C:\\Program Files\\Git\\cmd\\gitk.exe",
                &ShellEnv::new(),
                &everything
            ),
            "C:\\Program Files\\Git\\cmd\\gitk.exe"
        );
    }

    #[test]
    fn runs_windows_executables_directly_without_a_shell() {
        let host = env(&[("PATH", ""), ("PATHEXT", ".COM;.EXE;.BAT;.CMD")]);
        let cache = CommandResolutionCache::new();
        let resolver = SpawnResolver {
            id: 0,
            resolve: &no_resolver(),
        };
        let command = resolve_spawn_command(
            "node.exe",
            &["script.js", "hello & goodbye"],
            &CommandAvailabilityOptions::default(),
            resolver,
            &ctx(Platform::Win32, &host, &cache, Instant::now()),
        );
        assert_eq!(
            command,
            ResolvedSpawnCommand {
                command: "node.exe".to_owned(),
                args: vec!["script.js".to_owned(), "hello & goodbye".to_owned()],
                shell: false,
            }
        );
    }

    #[test]
    fn builds_one_escaped_cmd_line_for_windows_command_shims() {
        let host = env(&[("PATH", ""), ("PATHEXT", ".COM;.EXE;.BAT;.CMD")]);
        let cache = CommandResolutionCache::new();
        let resolve = |_: &str, _: Platform, _: &ShellEnv| {
            Some("C:\\Program Files\\npm & tools\\vp.cmd".to_owned())
        };
        let command = resolve_spawn_command(
            "vp",
            &[
                "run",
                "value & calc",
                "%PATH%",
                "quote\"value",
                "C:\\dir with space\\",
                "slash\\\"quote",
                "x|y<z>^",
            ],
            &CommandAvailabilityOptions {
                env: Some(env(&[("PATH", ""), ("PATHEXT", ".COM;.EXE;.BAT;.CMD")])),
                extend_env: false,
            },
            SpawnResolver {
                id: 1,
                resolve: &resolve,
            },
            &ctx(Platform::Win32, &host, &cache, Instant::now()),
        );
        // No args alongside `shell: true`: Node prints DEP0190 for that pairing.
        assert_eq!(
            command,
            ResolvedSpawnCommand {
                command: [
                    r#"^"C:\Program^ Files\npm^ ^&^ tools\vp.cmd^""#,
                    r#"^"run^""#,
                    r#"^"value^ ^&^ calc^""#,
                    r#"^"^%PATH^%^""#,
                    r#"^"quote\^"value^""#,
                    r#"^"C:\dir^ with^ space\\^""#,
                    r#"^"slash\\\^"quote^""#,
                    r#"^"x^|y^<z^>^^^""#,
                ]
                .join(" "),
                args: Vec::new(),
                shell: true,
            }
        );
    }

    #[test]
    fn resolves_against_the_effective_environment_when_extending_host_env() {
        let host = env(&[
            ("PATH", "C:\\Users\\tester\\AppData\\Roaming\\npm"),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
        ]);
        let cache = CommandResolutionCache::new();
        let seen = std::cell::RefCell::new(None);
        let resolve = |_: &str, _: Platform, effective: &ShellEnv| {
            *seen.borrow_mut() = Some(effective.clone());
            Some("C:\\Users\\tester\\AppData\\Roaming\\npm\\codex.cmd".to_owned())
        };
        let _ = resolve_spawn_command(
            "codex",
            &["app-server"],
            &CommandAvailabilityOptions {
                env: Some(env(&[("CODEX_HOME", "C:\\Users\\tester\\.codex")])),
                extend_env: true,
            },
            SpawnResolver {
                id: 2,
                resolve: &resolve,
            },
            &ctx(Platform::Win32, &host, &cache, Instant::now()),
        );
        assert_eq!(
            seen.into_inner(),
            Some(env(&[
                ("PATH", "C:\\Users\\tester\\AppData\\Roaming\\npm"),
                ("PATHEXT", ".COM;.EXE;.BAT;.CMD"),
                ("CODEX_HOME", "C:\\Users\\tester\\.codex"),
            ]))
        );
    }

    #[test]
    fn scans_path_once_per_command_until_the_search_environment_changes() {
        let host = ShellEnv::new();
        let cache = CommandResolutionCache::new();
        let scans = std::cell::RefCell::new(Vec::<String>::new());
        let scan = |name: &str, _: Platform, env: &ShellEnv| {
            let path = env.get("PATH").cloned().unwrap_or_default();
            scans.borrow_mut().push(format!("{name}@{path}"));
            if name == "missing" {
                None
            } else {
                Some(format!("{path}\\{name}.exe"))
            }
        };
        let resolver = SpawnResolver {
            id: 3,
            resolve: &scan,
        };
        let start = Instant::now();
        let resolve = |command: &str, path: &str, at: Instant| {
            resolve_spawn_command(
                command,
                &[],
                &CommandAvailabilityOptions {
                    env: Some(env(&[("PATH", path), ("PATHEXT", ".EXE")])),
                    extend_env: false,
                },
                resolver,
                &ctx(Platform::Win32, &host, &cache, at),
            )
        };

        assert_eq!(resolve("git", "C:\\one", start).command, "C:\\one\\git.exe");
        assert_eq!(resolve("git", "C:\\one", start).command, "C:\\one\\git.exe");
        // A failed spawn is how a provider reports "not installed", so a miss
        // must clear the moment the binary appears.
        resolve("missing", "C:\\one", start);
        resolve("missing", "C:\\one", start);
        assert_eq!(resolve("git", "C:\\two", start).command, "C:\\two\\git.exe");
        // Callers probe explicit paths they may have just written.
        resolve("C:\\tools\\git.exe", "C:\\one", start);
        resolve("C:\\tools\\git.exe", "C:\\one", start);
        assert_eq!(
            *scans.borrow(),
            vec![
                "git@C:\\one".to_owned(),
                "missing@C:\\one".to_owned(),
                "missing@C:\\one".to_owned(),
                "git@C:\\two".to_owned(),
                "C:\\tools\\git.exe@C:\\one".to_owned(),
                "C:\\tools\\git.exe@C:\\one".to_owned(),
            ]
        );

        resolve("git", "C:\\one", start + Duration::from_secs(31));
        assert_eq!(scans.borrow().len(), 7);

        // Another resolver sharing the cache gets its own answer.
        let elsewhere = SpawnResolver {
            id: 4,
            resolve: &|_: &str, _: Platform, _: &ShellEnv| {
                Some("D:\\elsewhere\\git.exe".to_owned())
            },
        };
        let other = resolve_spawn_command(
            "git",
            &[],
            &CommandAvailabilityOptions {
                env: Some(env(&[("PATH", "C:\\one"), ("PATHEXT", ".EXE")])),
                extend_env: false,
            },
            elsewhere,
            &ctx(
                Platform::Win32,
                &host,
                &cache,
                start + Duration::from_secs(31),
            ),
        );
        assert_eq!(other.command, "D:\\elsewhere\\git.exe");
        assert_eq!(
            resolve("git", "C:\\one", start + Duration::from_secs(31)).command,
            "C:\\one\\git.exe"
        );
        assert_eq!(scans.borrow().len(), 7);
    }

    #[test]
    fn keeps_launcher_swaps_apart_for_environments_with_and_without_msystem() {
        let host = ShellEnv::new();
        let cache = CommandResolutionCache::new();
        let scans = std::cell::Cell::new(0);
        let scan = |_: &str, _: Platform, _: &ShellEnv| {
            scans.set(scans.get() + 1);
            Some("C:\\Git\\cmd\\git.exe".to_owned())
        };
        let resolver = SpawnResolver {
            id: 5,
            resolve: &scan,
        };
        let now = Instant::now();
        let resolve = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("PATH", "C:\\Git\\cmd"), ("PATHEXT", ".EXE")];
            pairs.extend_from_slice(extra);
            resolve_spawn_command(
                "git",
                &[],
                &CommandAvailabilityOptions {
                    env: Some(env(&pairs)),
                    extend_env: false,
                },
                resolver,
                &ctx(Platform::Win32, &host, &cache, now),
            )
        };
        resolve(&[]);
        resolve(&[]);
        assert_eq!(scans.get(), 1);
        // A swap made without MSYSTEM must not reach a child that has it set.
        resolve(&[("MSYSTEM", "MINGW64")]);
        assert_eq!(scans.get(), 2);
    }

    #[test]
    fn does_not_fall_back_to_a_shell_for_unresolved_windows_commands() {
        let host = env(&[("PATH", ""), ("PATHEXT", ".COM;.EXE;.BAT;.CMD")]);
        let cache = CommandResolutionCache::new();
        let command = resolve_spawn_command(
            "missing & calc",
            &["unsafe & value"],
            &CommandAvailabilityOptions::default(),
            SpawnResolver {
                id: 0,
                resolve: &no_resolver(),
            },
            &ctx(Platform::Win32, &host, &cache, Instant::now()),
        );
        assert_eq!(
            command,
            ResolvedSpawnCommand {
                command: "missing & calc".to_owned(),
                args: vec!["unsafe & value".to_owned()],
                shell: false,
            }
        );
    }

    fn profile_env() -> ShellEnv {
        env(&[
            ("PATH", "C:\\Windows\\System32"),
            ("APPDATA", "C:\\Users\\testuser\\AppData\\Roaming"),
            ("LOCALAPPDATA", "C:\\Users\\testuser\\AppData\\Local"),
            ("USERPROFILE", "C:\\Users\\testuser"),
        ])
    }

    #[test]
    fn uses_known_cli_directories_as_a_fallback_without_changing_shell_path_priority() {
        let calls = std::cell::RefCell::new(Vec::<bool>::new());
        let read = |_: &[&str], options: &WindowsEnvironmentProbeOptions| {
            calls.borrow_mut().push(options.load_profile);
            if options.load_profile {
                env(&[("PATH", "C:\\Profile\\Bin")])
            } else {
                env(&[("PATH", "C:\\Shell\\Bin;C:\\Windows\\System32")])
            }
        };
        let checked = std::cell::RefCell::new(Vec::<String>::new());
        let available = |command: &str, _: &ShellEnv| {
            checked.borrow_mut().push(command.to_owned());
            true
        };
        let patch = resolve_windows_environment(&profile_env(), &read, &available);
        assert_eq!(
            patch.get("PATH").map(String::as_str),
            Some(
                [
                    "C:\\Shell\\Bin",
                    "C:\\Windows\\System32",
                    "C:\\Users\\testuser\\AppData\\Roaming\\npm",
                    "C:\\Users\\testuser\\AppData\\Local\\Programs\\nodejs",
                    "C:\\Users\\testuser\\AppData\\Local\\Volta\\bin",
                    "C:\\Users\\testuser\\AppData\\Local\\pnpm",
                    "C:\\Users\\testuser\\.local\\bin",
                    "C:\\Users\\testuser\\.bun\\bin",
                    "C:\\Users\\testuser\\scoop\\shims",
                ]
                .join(";")
                .as_str()
            )
        );
        assert_eq!(*calls.borrow(), vec![false]);
        assert_eq!(*checked.borrow(), vec!["node".to_owned()]);
    }

    #[test]
    fn loads_the_powershell_profile_when_baseline_env_cannot_resolve_node() {
        let read = |_: &[&str], options: &WindowsEnvironmentProbeOptions| {
            if options.load_profile {
                env(&[
                    ("PATH", "C:\\Profile\\Node;C:\\Windows\\System32"),
                    ("FNM_DIR", "C:\\Users\\testuser\\AppData\\Roaming\\fnm"),
                    (
                        "FNM_MULTISHELL_PATH",
                        "C:\\Users\\testuser\\AppData\\Local\\fnm_multishells\\123",
                    ),
                ])
            } else {
                env(&[("PATH", "C:\\Shell\\Bin;C:\\Windows\\System32")])
            }
        };
        let checks = std::cell::Cell::new(0);
        let available = |_: &str, _: &ShellEnv| {
            checks.set(checks.get() + 1);
            false
        };
        let patch = resolve_windows_environment(&profile_env(), &read, &available);
        assert_eq!(
            patch,
            env(&[
                (
                    "PATH",
                    [
                        "C:\\Profile\\Node",
                        "C:\\Windows\\System32",
                        "C:\\Shell\\Bin",
                        "C:\\Users\\testuser\\AppData\\Roaming\\npm",
                        "C:\\Users\\testuser\\AppData\\Local\\Programs\\nodejs",
                        "C:\\Users\\testuser\\AppData\\Local\\Volta\\bin",
                        "C:\\Users\\testuser\\AppData\\Local\\pnpm",
                        "C:\\Users\\testuser\\.local\\bin",
                        "C:\\Users\\testuser\\.bun\\bin",
                        "C:\\Users\\testuser\\scoop\\shims",
                    ]
                    .join(";")
                    .as_str(),
                ),
                ("FNM_DIR", "C:\\Users\\testuser\\AppData\\Roaming\\fnm"),
                (
                    "FNM_MULTISHELL_PATH",
                    "C:\\Users\\testuser\\AppData\\Local\\fnm_multishells\\123"
                ),
            ])
        );
        assert_eq!(checks.get(), 1);
    }

    #[test]
    fn keeps_the_baseline_env_when_profiled_probe_still_does_not_resolve_node() {
        let read = |_: &[&str], options: &WindowsEnvironmentProbeOptions| {
            if options.load_profile {
                env(&[("FNM_DIR", "C:\\Users\\testuser\\AppData\\Roaming\\fnm")])
            } else {
                ShellEnv::new()
            }
        };
        let checks = std::cell::Cell::new(0);
        let available = |_: &str, _: &ShellEnv| {
            checks.set(checks.get() + 1);
            false
        };
        let patch = resolve_windows_environment(
            &env(&[
                ("PATH", "C:\\Windows\\System32"),
                ("APPDATA", "C:\\Users\\testuser\\AppData\\Roaming"),
                ("USERPROFILE", "C:\\Users\\testuser"),
            ]),
            &read,
            &available,
        );
        assert_eq!(
            patch,
            env(&[
                (
                    "PATH",
                    "C:\\Windows\\System32;C:\\Users\\testuser\\AppData\\Roaming\\npm;C:\\Users\\testuser\\.local\\bin;C:\\Users\\testuser\\.bun\\bin;C:\\Users\\testuser\\scoop\\shims",
                ),
                ("FNM_DIR", "C:\\Users\\testuser\\AppData\\Roaming\\fnm"),
            ])
        );
        assert_eq!(checks.get(), 1);
    }

    #[test]
    fn escape_windows_shell_arg_matches_cross_spawn_rules() {
        assert_eq!(escape_windows_shell_arg("plain"), "^\"plain^\"");
        assert_eq!(
            escape_windows_shell_arg("a\\\\\"b"),
            "^\"a\\\\\\\\\\^\"b^\""
        );
    }

    #[test]
    fn the_capture_name_pattern_matches_upper_snake_names_only() {
        assert!(is_valid_env_name("PATH"));
        assert!(is_valid_env_name("FNM_MULTISHELL_PATH2"));
        assert!(!is_valid_env_name(""));
        assert!(!is_valid_env_name("lower"));
        assert!(!is_valid_env_name("A-B"));
    }

    #[test]
    fn cache_evicts_the_oldest_entry_at_capacity() {
        let cache = CommandResolutionCache::new();
        let now = Instant::now();
        for index in 0..COMMAND_RESOLUTION_CACHE_MAX_ENTRIES {
            cache.store(format!("key-{index}"), None, now);
        }
        assert_eq!(cache.len(), COMMAND_RESOLUTION_CACHE_MAX_ENTRIES);
        cache.store("newest".to_owned(), None, now);
        assert_eq!(cache.len(), COMMAND_RESOLUTION_CACHE_MAX_ENTRIES);
        assert!(cache.fresh("key-0", now).is_none());
        assert!(cache.fresh("key-1", now).is_some());
    }
}

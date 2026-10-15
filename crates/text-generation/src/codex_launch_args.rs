//! Port of upstream/apps/server/src/provider/codexLaunchArgs.ts, exec half only.
//!
//! Copied from the T09d `codex_launch_args.rs` branch. Dedupe when `provider-codex` lands.

use crate::cli_args::tokenize_cli_args;

const T3CODE_CODEX_LAUNCH_ARGS_ENV: &str = "T3CODE_CODEX_LAUNCH_ARGS";

/// `resolveCodexLaunchArgs`: `T3CODE_CODEX_LAUNCH_ARGS` wins when it is set and non-blank;
/// otherwise the configured `launchArgs`. Both are trimmed.
pub fn resolve_codex_launch_args(
    launch_args: Option<&str>,
    environment: &dyn Fn(&str) -> Option<String>,
) -> String {
    let from_env = environment(T3CODE_CODEX_LAUNCH_ARGS_ENV)
        .map(|value| turin_contracts::js_trim(&value).to_owned())
        .filter(|value| !value.is_empty());
    from_env
        .or_else(|| launch_args.map(|value| turin_contracts::js_trim(value).to_owned()))
        .unwrap_or_default()
}

/// `codexExecLaunchArgs`: keeps the flags `codex exec` shares with the app server and drops
/// app-server-only flags such as `--listen`.
pub fn codex_exec_launch_args(launch_args: Option<&str>) -> Vec<String> {
    let args = tokenize_cli_args(launch_args);
    let mut exec_args = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if arg == "--strict-config" || arg.starts_with("--config=") || arg.starts_with("-c=") {
            exec_args.push(arg.to_owned());
        } else if matches!(arg, "--config" | "-c" | "--enable" | "--disable") {
            if let Some(value) = args.get(index + 1).filter(|value| !value.starts_with('-')) {
                exec_args.push(arg.to_owned());
                exec_args.push(value.clone());
                index += 1;
            }
        } else if arg.starts_with("--enable=") || arg.starts_with("--disable=") {
            exec_args.push(arg.to_owned());
        }
        index += 1;
    }
    exec_args
}

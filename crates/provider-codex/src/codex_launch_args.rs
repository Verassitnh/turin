//! Port of upstream/apps/server/src/provider/codexLaunchArgs.ts.
use turin_contracts::base_schemas::js_trim;
use turin_shared::cli_args::tokenize_cli_args;

const T3CODE_CODEX_LAUNCH_ARGS_ENV: &str = "T3CODE_CODEX_LAUNCH_ARGS";

/// `resolveCodexLaunchArgs`: `T3CODE_CODEX_LAUNCH_ARGS` wins when it is set and
/// non-blank; otherwise the configured `launchArgs`. Both are trimmed.
/// `env` looks up one variable; the driver passes the instance environment
/// first, then the process environment.
pub fn resolve_codex_launch_args(
    launch_args: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> String {
    let from_env = env(T3CODE_CODEX_LAUNCH_ARGS_ENV)
        .map(|value| js_trim(&value).to_owned())
        .filter(|value| !value.is_empty());
    from_env
        .or_else(|| launch_args.map(|value| js_trim(value).to_owned()))
        .unwrap_or_default()
}

/// `codexAppServerArgs`: `app-server` followed by the parsed launch args.
pub fn codex_app_server_args(launch_args: Option<&str>) -> Vec<String> {
    std::iter::once("app-server".to_owned())
        .chain(codex_launch_argv(launch_args))
        .collect()
}

/// `codexExecLaunchArgs`: keeps the flags `codex exec` shares with the app
/// server and drops app-server-only flags (`--listen`, ...).
pub fn codex_exec_launch_args(launch_args: Option<&str>) -> Vec<String> {
    let args = codex_launch_argv(launch_args);
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

fn codex_launch_argv(launch_args: Option<&str>) -> Vec<String> {
    tokenize_cli_args(launch_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_with(value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |_| Some(value.to_owned())
    }

    #[test]
    fn env_var_wins_over_settings() {
        assert_eq!(
            resolve_codex_launch_args(Some(" --strict-config "), env_with(" --enable foo ")),
            "--enable foo"
        );
    }

    #[test]
    fn settings_used_when_env_var_is_blank() {
        assert_eq!(
            resolve_codex_launch_args(Some(" --strict-config "), env_with("   ")),
            "--strict-config"
        );
    }

    #[test]
    fn blank_env_and_no_settings_is_empty() {
        assert_eq!(resolve_codex_launch_args(Some(""), env_with("   ")), "");
        assert_eq!(resolve_codex_launch_args(None, no_env), "");
    }

    #[test]
    fn app_server_args_start_with_subcommand() {
        assert_eq!(codex_app_server_args(Some("")), vec!["app-server"]);
        assert_eq!(
            codex_app_server_args(Some("--strict-config --enable foo")),
            vec!["app-server", "--strict-config", "--enable", "foo"]
        );
    }

    #[test]
    fn exec_args_keep_shared_flags_and_drop_app_server_flags() {
        assert_eq!(
            codex_exec_launch_args(Some(
                r#"--strict-config --enable foo --listen off --config model="gpt 5""#
            )),
            vec![
                "--strict-config",
                "--enable",
                "foo",
                "--config",
                "model=gpt 5"
            ]
        );
    }

    #[test]
    fn exec_args_do_not_pair_value_flags_with_adjacent_flags() {
        assert_eq!(
            codex_exec_launch_args(Some("--config --strict-config --enable --disable")),
            vec!["--strict-config"]
        );
    }
}

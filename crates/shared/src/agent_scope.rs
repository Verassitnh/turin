//! Port of upstream/packages/shared/src/AgentScope.ts
//!
//! The server supplies the real scope. The default leaves a command unchanged,
//! which is also the behavior on macOS, Windows, and in tests.

/// Agents get this `oom_score_adj` so the kernel kills them before the server.
pub const AGENT_OOM_SCORE_ADJ: u16 = 800;

/// Shell line that raises the score of the current process, ignoring failure.
pub const RAISE_OOM_SCORE_LINE: &str =
    "{ echo 800 >/proc/self/oom_score_adj; } 2>/dev/null";

const LAUNCH_SCRIPT: &str = "{ echo 800 >/proc/self/oom_score_adj; } 2>/dev/null\nexec \"$@\"";

const SCOPED_LAUNCH_SCRIPT: &str = "{ echo 800 >/proc/self/oom_score_adj; } 2>/dev/null\n: \"${XDG_RUNTIME_DIR:=$1}\"\nexport XDG_RUNTIME_DIR\nshift\nexec \"$@\"";

pub struct AgentScopeCommand {
    pub command: String,
    pub args: Vec<String>,
}

pub struct AgentSystemdScope {
    pub systemd_run: String,
    pub slice: String,
    pub unit: String,
    pub runtime_dir: String,
    pub options: Vec<String>,
}

/// Default `AgentScope` service: commands stay as given.
pub fn passthrough_agent_scope(command: &str, args: &[impl AsRef<str>]) -> AgentScopeCommand {
    AgentScopeCommand {
        command: command.to_owned(),
        args: args.iter().map(|arg| arg.as_ref().to_owned()).collect(),
    }
}

pub fn default_oom_killed(_thread_id: &str) -> bool {
    false
}

/// Linux launch for an agent command. Every step `exec`s, so the spawned PID
/// belongs to the agent itself.
pub fn agent_scope_command(
    command: &str,
    args: &[impl AsRef<str>],
    scope: Option<&AgentSystemdScope>,
) -> AgentScopeCommand {
    let tail = std::iter::once(command.to_owned()).chain(args.iter().map(|arg| arg.as_ref().to_owned()));
    match scope {
        None => AgentScopeCommand {
            command: "/bin/sh".to_owned(),
            args: ["-c", LAUNCH_SCRIPT, "t3-agent"]
                .into_iter()
                .map(str::to_owned)
                .chain(tail)
                .collect(),
        },
        Some(scope) => {
            let mut argv = vec![
                "-c".to_owned(),
                SCOPED_LAUNCH_SCRIPT.to_owned(),
                "t3-agent".to_owned(),
                scope.runtime_dir.clone(),
                scope.systemd_run.clone(),
                "--user".to_owned(),
                "--scope".to_owned(),
                "--quiet".to_owned(),
                format!("--slice={}", scope.slice),
                format!("--unit={}", scope.unit),
            ];
            argv.extend(scope.options.iter().cloned());
            argv.push("--".to_owned());
            argv.extend(tail);
            AgentScopeCommand {
                command: "/bin/sh".to_owned(),
                args: argv,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scope_leaves_the_command_unchanged() {
        let command = passthrough_agent_scope("claude", &["--resume"]);
        assert_eq!(command.command, "claude");
        assert_eq!(command.args, ["--resume"]);
        assert!(!default_oom_killed("thread"));
        assert_eq!(RAISE_OOM_SCORE_LINE.contains("800"), true);
        assert_eq!(AGENT_OOM_SCORE_ADJ, 800);
    }

    #[test]
    fn unscoped_launch_raises_the_score_then_execs() {
        let launch = agent_scope_command("/bin/echo", &["hi"], None);
        assert_eq!(launch.command, "/bin/sh");
        assert_eq!(
            launch.args,
            [
                "-c",
                "{ echo 800 >/proc/self/oom_score_adj; } 2>/dev/null\nexec \"$@\"",
                "t3-agent",
                "/bin/echo",
                "hi",
            ]
        );
    }

    #[test]
    fn scoped_launch_passes_the_runtime_dir_to_systemd_run() {
        let scope = AgentSystemdScope {
            systemd_run: "systemd-run".into(),
            slice: "t3-agents.slice".into(),
            unit: "claude-1.scope".into(),
            runtime_dir: "/run/user/1".into(),
            options: vec!["--property=OOMPolicy=stop".into()],
        };
        let launch = agent_scope_command("claude", &["--resume"], Some(&scope));
        assert_eq!(launch.args[3], "/run/user/1");
        assert!(launch.args.contains(&"--property=OOMPolicy=stop".to_owned()));
        assert!(launch.args.contains(&"--slice=t3-agents.slice".to_owned()));
        assert_eq!(launch.args[launch.args.len() - 2], "claude");
        assert_eq!(launch.args[launch.args.len() - 1], "--resume");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn raises_the_oom_score_and_execs_in_the_same_process() {
        let launch = agent_scope_command(
            "/bin/sh",
            &[
                "-c",
                "printf \"%s\\n\" \"$$\" \"$(cat /proc/self/oom_score_adj)\" \"$@\"",
                "agent",
                "a b",
                "$HOME",
            ],
            None,
        );
        let child = std::process::Command::new(&launch.command)
            .args(&launch.args)
            .output()
            .unwrap();
        let stdout = String::from_utf8(child.stdout).unwrap();
        let mut lines = stdout.trim_end().split('\n');
        let pid: u32 = lines.next().unwrap().parse().unwrap();
        let score: u16 = lines.next().unwrap().trim().parse().unwrap();
        let args: Vec<&str> = lines.collect();
        assert_eq!(pid, child.id());
        assert_eq!(score, AGENT_OOM_SCORE_ADJ);
        assert_eq!(args, ["a b", "$HOME"]);
    }
}

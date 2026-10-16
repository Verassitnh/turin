//! Port of upstream/apps/server/src/orchestration-v2/workflowScriptQuery.ts
//!
//! Serves a workflow script the harness wrote under the Claude projects root.
//! Containment is `read_contained_workflow_file`'s; this module only maps its
//! reasons onto the script error vocabulary (`wrong-extension` becomes
//! `not-js`).

use turin_contracts::orchestration_v2::{
    OrchestrationGetWorkflowScriptError, OrchestrationV2WorkflowFileErrorReason as FileReason,
    OrchestrationV2WorkflowScriptErrorReason as ScriptReason, OrchestrationWorkflowFileError,
};

use super::workflow_file_read::{ContainedWorkflowFileRequest, read_contained_workflow_file};

const SCRIPT_BYTE_CAP: usize = 256 * 1024;

/// The script as served: its resolved path and capped contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowScript {
    pub script_path: String,
    pub contents: String,
    pub truncated: bool,
}

/// `readWorkflowScript`. `home_directory` is `HostProcess.HomeDirectory`.
pub async fn read_workflow_script(
    script_path: &str,
    home_directory: &str,
) -> Result<WorkflowScript, OrchestrationGetWorkflowScriptError> {
    let file = read_contained_workflow_file(
        ContainedWorkflowFileRequest {
            path: script_path,
            extension: ".js",
            byte_cap: SCRIPT_BYTE_CAP,
            tail: false,
            config_dir: None,
        },
        home_directory,
    )
    .await
    .map_err(|error| {
        let OrchestrationWorkflowFileError::OrchestrationWorkflowFileError { reason, path, .. } =
            &error;
        OrchestrationGetWorkflowScriptError::OrchestrationGetWorkflowScriptError {
            reason: script_reason(*reason),
            script_path: path.clone(),
            cause: serde_json::to_value(&error).ok(),
        }
    })?;
    Ok(WorkflowScript {
        script_path: file.path,
        contents: file.contents,
        truncated: file.truncated,
    })
}

fn script_reason(reason: FileReason) -> ScriptReason {
    match reason {
        FileReason::InvalidPath => ScriptReason::InvalidPath,
        FileReason::RootUnavailable => ScriptReason::RootUnavailable,
        FileReason::NotFound => ScriptReason::NotFound,
        FileReason::OutsideRoot => ScriptReason::OutsideRoot,
        FileReason::WrongExtension => ScriptReason::NotJs,
        FileReason::NotRegularFile => ScriptReason::NotRegularFile,
        FileReason::ChangedDuringRead => ScriptReason::ChangedDuringRead,
        FileReason::ReadFailed => ScriptReason::ReadFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake home with a projects root holding the script, plus a file outside
    /// the root for escape checks. Nothing touches the real `~/.claude`.
    struct Fixture {
        _temp: tempfile::TempDir,
        home: String,
        root: std::path::PathBuf,
        script_path: String,
        outside: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home
            .join(".claude")
            .join("projects")
            .join("__wf_script_test__");
        std::fs::create_dir_all(&root).unwrap();
        let script = root.join("run.js");
        std::fs::write(&script, "export const meta = {};\n").unwrap();
        let outside = temp.path().join("wf-outside.js");
        std::fs::write(&outside, "evil\n").unwrap();
        Fixture {
            home: home.to_string_lossy().into_owned(),
            script_path: script.to_string_lossy().into_owned(),
            root,
            outside,
            _temp: temp,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_fifo_without_waiting_for_a_writer() {
        let fixture = fixture();
        let fifo = fixture.root.join("pipe.js");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        let error = read_workflow_script(&fifo.to_string_lossy(), &fixture.home)
            .await
            .unwrap_err();
        let OrchestrationGetWorkflowScriptError::OrchestrationGetWorkflowScriptError {
            reason, ..
        } = error;
        assert_eq!(reason, ScriptReason::NotRegularFile);
    }

    #[tokio::test]
    async fn serves_a_real_script_under_the_projects_root() {
        let fixture = fixture();
        let result = read_workflow_script(&fixture.script_path, &fixture.home)
            .await
            .unwrap();
        assert!(result.contents.contains("export const meta"));
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn rejects_relative_and_non_js_paths() {
        let fixture = fixture();
        assert!(read_workflow_script("run.js", &fixture.home).await.is_err());
        assert!(
            read_workflow_script(&fixture.script_path.replace(".js", ".ts"), &fixture.home)
                .await
                .is_err()
        );
        let error = read_workflow_script("run.js", &fixture.home)
            .await
            .unwrap_err();
        let OrchestrationGetWorkflowScriptError::OrchestrationGetWorkflowScriptError {
            reason,
            script_path,
            cause,
        } = &error;
        assert_eq!(script_path, "run.js");
        assert_eq!(*reason, ScriptReason::InvalidPath);
        assert_eq!(
            error.to_string(),
            "Workflow scripts must be absolute .js paths."
        );
        let cause: OrchestrationWorkflowFileError =
            serde_json::from_value(cause.clone().unwrap()).unwrap();
        assert!(matches!(
            cause,
            OrchestrationWorkflowFileError::OrchestrationWorkflowFileError { .. }
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_paths_outside_the_root_and_symlink_escapes() {
        let fixture = fixture();
        let escaped = read_workflow_script(&fixture.outside.to_string_lossy(), &fixture.home).await;
        assert!(escaped.is_err());
        // A symlink INSIDE the root pointing outside must fail specifically on
        // realpath re-containment — a "not-found" would mean the link was
        // never exercised and the assertion proves nothing.
        let link = fixture.root.join("sneaky.js");
        std::os::unix::fs::symlink(&fixture.outside, &link).unwrap();
        let error = read_workflow_script(&link.to_string_lossy(), &fixture.home)
            .await
            .unwrap_err();
        let OrchestrationGetWorkflowScriptError::OrchestrationGetWorkflowScriptError {
            reason, ..
        } = error;
        assert_eq!(reason, ScriptReason::OutsideRoot);
    }
}

//! Test-only scripted `SourceControlHost`: every operation is an optional closure, calls are
//! recorded, and an operation with no script panics (the upstream `TestSourceControlHost`
//! "Unexpected … in a test that supplied none" behaviour).

#![allow(dead_code)] // scripted helpers the service tests still to be ported will use

use async_trait::async_trait;
use parking_lot::Mutex;
use turin_contracts::git::{GitCommandError, VcsSwitchRefInput, VcsSwitchRefResult};
use turin_contracts::settings::{ServerSettings, ServerSettingsError};
use turin_contracts::vcs::{VcsError, VcsListRemotesResult};

use super::host::{
    SourceControlGitExecuteInput, SourceControlGitOutput, SourceControlGitRemoteBranch,
    SourceControlHost, SourceControlProcessInput, SourceControlProcessOutput,
};

type Handler<I, O> = Box<dyn Fn(I) -> O + Send + Sync>;

#[derive(Default)]
#[allow(clippy::type_complexity)] // each scripted operation is its own handler type
pub(crate) struct ScriptedHost {
    pub settings: Mutex<ServerSettings>,
    pub process:
        Option<Handler<SourceControlProcessInput, Result<SourceControlProcessOutput, VcsError>>>,
    pub git_execute: Option<
        Handler<SourceControlGitExecuteInput, Result<SourceControlGitOutput, GitCommandError>>,
    >,
    pub resolve_commit: Option<Handler<(String, String), Result<String, GitCommandError>>>,
    pub remotes: Option<Handler<String, Result<VcsListRemotesResult, VcsError>>>,
    pub read_config_value:
        Option<Handler<(String, String), Result<Option<String>, GitCommandError>>>,
    pub resolve_primary_remote_name: Option<Handler<String, Result<String, GitCommandError>>>,
    pub ensure_remote: Option<Handler<(String, String, String), Result<String, GitCommandError>>>,
    pub list_local_branch_names: Option<Handler<String, Result<Vec<String>, GitCommandError>>>,
    pub fetch_remote_branch:
        Option<Handler<(SourceControlGitRemoteBranch, String), Result<(), GitCommandError>>>,
    pub fetch_remote_tracking_branch:
        Option<Handler<SourceControlGitRemoteBranch, Result<(), GitCommandError>>>,
    pub set_branch_upstream:
        Option<Handler<(SourceControlGitRemoteBranch, String), Result<(), GitCommandError>>>,
    pub switch_ref: Option<Handler<VcsSwitchRefInput, Result<VcsSwitchRefResult, GitCommandError>>>,
    /// `(name, arguments)` of every call a script chose to record, in order.
    pub calls: Mutex<Vec<(String, serde_json::Value)>>,
}

impl ScriptedHost {
    pub fn record(&self, name: &str, args: serde_json::Value) {
        self.calls.lock().push((name.to_owned(), args));
    }
    pub fn call_names(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(n, _)| n.clone()).collect()
    }
}

macro_rules! scripted {
    ($self:ident, $field:ident, $name:literal, $arg:expr) => {{
        match &$self.$field {
            Some(run) => run($arg),
            None => panic!(concat!(
                "Unexpected git ",
                $name,
                " in a test that supplied none."
            )),
        }
    }};
}

#[async_trait]
impl SourceControlHost for ScriptedHost {
    async fn settings_get(&self) -> Result<ServerSettings, ServerSettingsError> {
        Ok(self.settings.lock().clone())
    }
    async fn process_run(
        &self,
        input: SourceControlProcessInput,
    ) -> Result<SourceControlProcessOutput, VcsError> {
        match &self.process {
            Some(run) => run(input),
            None => panic!("Unexpected {} run in {}.", input.command, input.operation),
        }
    }
    async fn git_execute(
        &self,
        input: SourceControlGitExecuteInput,
    ) -> Result<SourceControlGitOutput, GitCommandError> {
        scripted!(self, git_execute, "execute", input)
    }
    async fn git_resolve_commit(
        &self,
        cwd: &str,
        revision: &str,
    ) -> Result<String, GitCommandError> {
        scripted!(
            self,
            resolve_commit,
            "resolveCommit",
            (cwd.to_owned(), revision.to_owned())
        )
    }
    async fn git_remotes(&self, cwd: &str) -> Result<VcsListRemotesResult, VcsError> {
        match &self.remotes {
            Some(run) => run(cwd.to_owned()),
            None => panic!("Unexpected git remotes in a test that supplied none."),
        }
    }
    async fn git_read_config_value(
        &self,
        cwd: &str,
        key: &str,
    ) -> Result<Option<String>, GitCommandError> {
        scripted!(
            self,
            read_config_value,
            "readConfigValue",
            (cwd.to_owned(), key.to_owned())
        )
    }
    async fn git_resolve_primary_remote_name(&self, cwd: &str) -> Result<String, GitCommandError> {
        scripted!(
            self,
            resolve_primary_remote_name,
            "resolvePrimaryRemoteName",
            cwd.to_owned()
        )
    }
    async fn git_ensure_remote(
        &self,
        cwd: &str,
        preferred_name: &str,
        url: &str,
    ) -> Result<String, GitCommandError> {
        scripted!(
            self,
            ensure_remote,
            "ensureRemote",
            (cwd.to_owned(), preferred_name.to_owned(), url.to_owned())
        )
    }
    async fn git_list_local_branch_names(&self, cwd: &str) -> Result<Vec<String>, GitCommandError> {
        scripted!(
            self,
            list_local_branch_names,
            "listLocalBranchNames",
            cwd.to_owned()
        )
    }
    async fn git_fetch_remote_branch(
        &self,
        input: &SourceControlGitRemoteBranch,
        local_branch: &str,
    ) -> Result<(), GitCommandError> {
        scripted!(
            self,
            fetch_remote_branch,
            "fetchRemoteBranch",
            (input.clone(), local_branch.to_owned())
        )
    }
    async fn git_fetch_remote_tracking_branch(
        &self,
        input: &SourceControlGitRemoteBranch,
    ) -> Result<(), GitCommandError> {
        scripted!(
            self,
            fetch_remote_tracking_branch,
            "fetchRemoteTrackingBranch",
            input.clone()
        )
    }
    async fn git_set_branch_upstream(
        &self,
        input: &SourceControlGitRemoteBranch,
        branch: &str,
    ) -> Result<(), GitCommandError> {
        scripted!(
            self,
            set_branch_upstream,
            "setBranchUpstream",
            (input.clone(), branch.to_owned())
        )
    }
    async fn git_switch_ref(
        &self,
        input: &VcsSwitchRefInput,
    ) -> Result<VcsSwitchRefResult, GitCommandError> {
        scripted!(self, switch_ref, "switchRef", input.clone())
    }
}

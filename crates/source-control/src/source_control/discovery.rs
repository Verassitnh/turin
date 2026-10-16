//! Port of upstream/apps/server/src/sourceControl/SourceControlDiscovery.ts
//!
//! Reports which version control systems (git, jj) and source control providers (GitHub) this
//! machine can use.

use std::sync::Arc;

use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::source_control::{
    SourceControlDiscoveryResult, SourceControlDiscoveryStatus, VcsDiscoveryItem,
};
use turin_contracts::vcs::VcsDriverKind;

use super::provider_registry::SourceControlProviderRegistry;
use crate::source_control_core::discovery::{detail_from_cause, first_non_empty_line, option_text};
use crate::source_control_core::host::{SourceControlHost, SourceControlProcessInput};

struct VcsProbe {
    kind: VcsDriverKind,
    label: &'static str,
    executable: &'static str,
    version_args: &'static [&'static str],
    implemented: bool,
    install_hint: &'static str,
}

const VCS_PROBES: &[VcsProbe] = &[
    VcsProbe {
        kind: VcsDriverKind::Git,
        label: "Git",
        executable: "git",
        version_args: &["--version"],
        implemented: true,
        install_hint: "Install Git from https://git-scm.com/downloads or with your package manager.",
    },
    VcsProbe {
        kind: VcsDriverKind::Jj,
        label: "Jujutsu",
        executable: "jj",
        version_args: &["--version"],
        implemented: false,
        install_hint: "Install Jujutsu with `brew install jj` or from https://github.com/jj-vcs/jj.",
    },
];

fn text(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value)
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("non-empty"))
}

pub struct SourceControlDiscovery {
    host: Arc<dyn SourceControlHost>,
    providers: Arc<SourceControlProviderRegistry>,
    /// `ServerConfig.cwd`.
    cwd: String,
}

impl SourceControlDiscovery {
    pub fn new(
        host: Arc<dyn SourceControlHost>,
        providers: Arc<SourceControlProviderRegistry>,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            host,
            providers,
            cwd: cwd.into(),
        }
    }

    async fn probe(&self, probe: &VcsProbe) -> VcsDiscoveryItem {
        let result = self
            .host
            .process_run(SourceControlProcessInput {
                operation: "source-control.discovery.probe".into(),
                command: probe.executable.into(),
                args: probe.version_args.iter().map(|s| (*s).to_owned()).collect(),
                cwd: self.cwd.clone(),
                timeout_ms: Some(5_000),
                max_output_bytes: Some(8_000),
                append_truncation_marker: true,
                ..Default::default()
            })
            .await;
        let (status, version, detail) = match result {
            Ok(result) => (
                SourceControlDiscoveryStatus::Available,
                first_non_empty_line(&result.stdout)
                    .or_else(|| first_non_empty_line(&result.stderr)),
                None,
            ),
            Err(cause) => (
                SourceControlDiscoveryStatus::Missing,
                None,
                detail_from_cause(&cause),
            ),
        };
        VcsDiscoveryItem {
            kind: probe.kind,
            implemented: probe.implemented,
            label: text(probe.label),
            executable: Some(text(probe.executable)),
            status,
            version: option_text(version.as_deref()),
            install_hint: text(probe.install_hint),
            detail: option_text(detail.as_deref()),
        }
    }

    /// Version control systems and source control providers, probed concurrently.
    pub async fn discover(&self) -> SourceControlDiscoveryResult {
        let (version_control_systems, source_control_providers) = tokio::join!(
            futures::future::join_all(VCS_PROBES.iter().map(|probe| self.probe(probe))),
            self.providers.discover(),
        );
        SourceControlDiscoveryResult {
            version_control_systems,
            source_control_providers,
        }
    }
}

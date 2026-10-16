//! Port of upstream/packages/source-control-core/src/server/discovery.ts
//!
//! Discovery specs: how each provider reports its CLI or API, and the shared probe that runs them.

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use regex::Regex;
use turin_contracts::TrimmedNonEmptyString;
use turin_contracts::base_schemas::EffectOption;
use turin_contracts::source_control::{
    SourceControlDiscoveryStatus, SourceControlProviderAuth, SourceControlProviderAuthStatus,
    SourceControlProviderDiscoveryItem, SourceControlProviderInfo, SourceControlProviderKind,
};

use super::host::{SourceControlHost, SourceControlProcessInput};
use super::source_control_provider::SourceControlProviderContext;

#[derive(Debug, Clone)]
pub struct SourceControlAuthProbeInput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone)]
pub struct SourceControlUnknownRemoteRefinementInput {
    pub cwd: String,
    pub context: SourceControlProviderContext,
    pub auth: SourceControlAuthProbeInput,
}

pub type ParseAuth =
    Arc<dyn Fn(&SourceControlAuthProbeInput) -> SourceControlProviderAuth + Send + Sync>;
pub type RefineUnknownRemote = Arc<
    dyn Fn(&SourceControlUnknownRemoteRefinementInput) -> Option<SourceControlProviderInfo>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct SourceControlCliDiscoverySpec {
    pub kind: SourceControlProviderKind,
    pub label: String,
    pub install_hint: String,
    pub executable: String,
    pub version_args: Vec<String>,
    pub auth_args: Vec<String>,
    pub remote_refinement_args: Option<Vec<String>>,
    pub probe_timeout_ms: Option<u64>,
    pub parse_auth: ParseAuth,
    pub refine_unknown_remote: Option<RefineUnknownRemote>,
}

#[derive(Clone)]
pub struct SourceControlApiDiscoverySpec {
    pub kind: SourceControlProviderKind,
    pub label: String,
    pub install_hint: String,
    pub probe_auth: Arc<dyn ProbeAuth>,
}

#[async_trait]
pub trait ProbeAuth: Send + Sync {
    async fn probe_auth(&self) -> SourceControlProviderAuth;
}

/// A provider whose probing and remote refinement run its own code (GitHub: settings + API).
#[async_trait]
pub trait ManagedCliProbe: Send + Sync {
    async fn probe(&self, cwd: &str) -> SourceControlProviderDiscoveryItem;
    async fn refine_unknown_remote(
        &self,
        cwd: &str,
        context: &SourceControlProviderContext,
    ) -> Option<SourceControlProviderInfo>;
}

#[derive(Clone)]
pub struct SourceControlManagedCliDiscoverySpec {
    pub kind: SourceControlProviderKind,
    pub label: String,
    pub install_hint: String,
    pub probe: Arc<dyn ManagedCliProbe>,
}

#[derive(Clone)]
pub enum SourceControlProviderDiscoverySpec {
    Cli(SourceControlCliDiscoverySpec),
    ManagedCli(SourceControlManagedCliDiscoverySpec),
    Api(SourceControlApiDiscoverySpec),
}

impl SourceControlProviderDiscoverySpec {
    pub fn kind(&self) -> &SourceControlProviderKind {
        match self {
            Self::Cli(s) => &s.kind,
            Self::ManagedCli(s) => &s.kind,
            Self::Api(s) => &s.kind,
        }
    }
    pub fn label(&self) -> &str {
        match self {
            Self::Cli(s) => &s.label,
            Self::ManagedCli(s) => &s.label,
            Self::Api(s) => &s.label,
        }
    }
    pub fn install_hint(&self) -> &str {
        match self {
            Self::Cli(s) => &s.install_hint,
            Self::ManagedCli(s) => &s.install_hint,
            Self::Api(s) => &s.install_hint,
        }
    }
}

// Most provider CLIs answer `--version` in well under a second, so a short budget keeps
// discovery snappy. Specs whose CLI is known to be slower can raise it via probe_timeout_ms.
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 5_000;

fn probe_timeout_ms(spec: &SourceControlCliDiscoverySpec) -> u64 {
    spec.probe_timeout_ms.unwrap_or(DEFAULT_PROBE_TIMEOUT_MS)
}

struct DiscoveryProbeResult {
    kind: SourceControlProviderKind,
    label: String,
    executable: String,
    status: SourceControlDiscoveryStatus,
    version: Option<String>,
    install_hint: String,
    detail: Option<String>,
}

static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    // Node's stripVTControlCharacters pattern: CSI/OSC ANSI escape sequences.
    Regex::new(
        "[\u{1b}\u{9b}][\\[\\]()#;?]*(?:(?:(?:(?:;[-a-zA-Z\\d/#&.:=?%@~_]+)*|[a-zA-Z\\d]+(?:;[-a-zA-Z\\d/#&.:=?%@~_]*)*)?\u{7})|(?:(?:\\d{1,4}(?:;\\d{0,4})*)?[\\dA-PR-TZcf-ntqry=><~]))",
    )
    .unwrap()
});
static TOKEN_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[-\s]*token(?:\s+scopes?)?:").unwrap());
static LEADING_NON_ALNUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[^a-z0-9]+").unwrap());
static HOST_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z0-9][a-z0-9.-]*(?::\d+)?$").unwrap());

fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// The first non-empty line, after stripping terminal control sequences.
pub fn first_non_empty_line(text: &str) -> Option<String> {
    let stripped = ANSI.replace_all(text, "");
    stripped
        .split('\n')
        .map(|l| js_trim(l.strip_suffix('\r').unwrap_or(l)).to_owned())
        .find(|l| !l.is_empty())
}

/// `detailFromCause`: the trimmed message of an error, if it has one.
pub fn detail_from_cause(cause: &dyn std::fmt::Display) -> Option<String> {
    let message = cause.to_string();
    let trimmed = js_trim(&message);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn optional_text(value: Option<&str>) -> EffectOption<TrimmedNonEmptyString> {
    match value
        .map(js_trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| TrimmedNonEmptyString::new(v).ok())
    {
        Some(v) => EffectOption::some(v),
        None => EffectOption::None,
    }
}

pub fn option_text(value: Option<&str>) -> EffectOption<TrimmedNonEmptyString> {
    optional_text(value)
}

pub struct ProviderAuthInput<'a> {
    pub status: SourceControlProviderAuthStatus,
    pub account: Option<&'a str>,
    pub host: Option<&'a str>,
    pub detail: Option<&'a str>,
}

pub fn provider_auth(input: ProviderAuthInput<'_>) -> SourceControlProviderAuth {
    SourceControlProviderAuth {
        status: input.status,
        account: optional_text(input.account),
        host: optional_text(input.host),
        detail: optional_text(input.detail),
        accounts: None,
    }
}

fn unknown_auth(detail: Option<&str>) -> SourceControlProviderAuth {
    provider_auth(ProviderAuthInput {
        status: SourceControlProviderAuthStatus::Unknown,
        account: None,
        host: None,
        detail,
    })
}

pub fn combined_auth_output(input: &SourceControlAuthProbeInput) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for entry in [&input.stdout, &input.stderr] {
        if !js_trim(entry).is_empty() {
            parts.push(entry);
        }
    }
    parts.join("\n")
}

fn sanitized_auth_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|entry| js_trim(entry.strip_suffix('\r').unwrap_or(entry)).to_owned())
        .filter(|line| !line.is_empty() && !TOKEN_LINE.is_match(line))
        .collect()
}

pub fn first_safe_auth_line(text: &str) -> Option<String> {
    sanitized_auth_lines(text).into_iter().next()
}

pub fn parse_cli_host(text: &str) -> Option<String> {
    sanitized_auth_lines(text)
        .into_iter()
        .map(|line| LEADING_NON_ALNUM.replace(&line, "").into_owned())
        .find(|line| HOST_LINE.is_match(line))
}

/// The first pattern whose first capture group is non-empty after trimming.
pub fn match_first(text: &str, patterns: &[Regex]) -> Option<String> {
    for pattern in patterns {
        if let Some(value) = pattern
            .captures(text)
            .and_then(|m| m.get(1))
            .map(|m| js_trim(m.as_str()))
            .filter(|v| !v.is_empty())
        {
            return Some(value.to_owned());
        }
    }
    None
}

fn cli_probe_input(
    spec: &SourceControlCliDiscoverySpec,
    cwd: &str,
    operation: &str,
    args: &[String],
    allow_non_zero_exit: bool,
) -> SourceControlProcessInput {
    SourceControlProcessInput {
        operation: operation.to_owned(),
        command: spec.executable.clone(),
        args: args.to_vec(),
        cwd: cwd.to_owned(),
        timeout_ms: Some(probe_timeout_ms(spec)),
        max_output_bytes: Some(8_000),
        append_truncation_marker: true,
        allow_non_zero_exit,
        ..Default::default()
    }
}

async fn probe_cli(
    spec: &SourceControlCliDiscoverySpec,
    host: &dyn SourceControlHost,
    cwd: &str,
) -> DiscoveryProbeResult {
    let result = host
        .process_run(cli_probe_input(
            spec,
            cwd,
            "source-control.discovery.probe",
            &spec.version_args,
            false,
        ))
        .await;
    match result {
        Ok(result) => DiscoveryProbeResult {
            kind: spec.kind.clone(),
            label: spec.label.clone(),
            executable: spec.executable.clone(),
            status: SourceControlDiscoveryStatus::Available,
            version: first_non_empty_line(&result.stdout)
                .or_else(|| first_non_empty_line(&result.stderr)),
            install_hint: spec.install_hint.clone(),
            detail: None,
        },
        Err(cause) => DiscoveryProbeResult {
            kind: spec.kind.clone(),
            label: spec.label.clone(),
            executable: spec.executable.clone(),
            status: SourceControlDiscoveryStatus::Missing,
            version: None,
            install_hint: spec.install_hint.clone(),
            detail: detail_from_cause(&cause),
        },
    }
}

fn item_text(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value)
        .unwrap_or_else(|_| TrimmedNonEmptyString::new("unknown").expect("non-empty"))
}

fn to_item(
    probe: DiscoveryProbeResult,
    auth: SourceControlProviderAuth,
) -> SourceControlProviderDiscoveryItem {
    SourceControlProviderDiscoveryItem {
        kind: probe.kind,
        label: item_text(&probe.label),
        executable: Some(item_text(&probe.executable)),
        status: probe.status,
        version: optional_text(probe.version.as_deref()),
        install_hint: item_text(&probe.install_hint),
        detail: optional_text(probe.detail.as_deref()),
        auth,
    }
}

pub async fn probe_source_control_provider(
    spec: &SourceControlProviderDiscoverySpec,
    host: &dyn SourceControlHost,
    cwd: &str,
) -> SourceControlProviderDiscoveryItem {
    match spec {
        SourceControlProviderDiscoverySpec::ManagedCli(spec) => spec.probe.probe(cwd).await,
        SourceControlProviderDiscoverySpec::Api(spec) => {
            let auth = spec.probe_auth.probe_auth().await;
            SourceControlProviderDiscoveryItem {
                kind: spec.kind.clone(),
                label: item_text(&spec.label),
                executable: None,
                status: SourceControlDiscoveryStatus::Available,
                version: EffectOption::None,
                install_hint: item_text(&spec.install_hint),
                detail: EffectOption::None,
                auth,
            }
        }
        SourceControlProviderDiscoverySpec::Cli(spec) => {
            let item = probe_cli(spec, host, cwd).await;
            if item.status != SourceControlDiscoveryStatus::Available {
                return to_item(
                    item,
                    unknown_auth(Some(
                        "Hosting integration command was not found on the server PATH.",
                    )),
                );
            }
            let auth = match host
                .process_run(cli_probe_input(
                    spec,
                    cwd,
                    "source-control.discovery.auth",
                    &spec.auth_args,
                    true,
                ))
                .await
            {
                Ok(result) => (spec.parse_auth)(&SourceControlAuthProbeInput {
                    stdout: result.stdout,
                    stderr: result.stderr,
                    exit_code: result.exit_code,
                }),
                Err(cause) => unknown_auth(detail_from_cause(&cause).as_deref()),
            };
            to_item(item, auth)
        }
    }
}

/// Fills in the provider of an `unknown` remote from whichever spec claims it. Specs are asked
/// one after another, as upstream's `Effect.forEach` without concurrency does.
pub async fn refine_unknown_remote_provider(
    specs: &[SourceControlProviderDiscoverySpec],
    host: &dyn SourceControlHost,
    cwd: &str,
    context: Option<SourceControlProviderContext>,
) -> Option<SourceControlProviderContext> {
    let context = context?;
    if context.provider.kind.as_str() != "unknown" {
        return Some(context);
    }
    let mut provider: Option<SourceControlProviderInfo> = None;
    for spec in specs {
        let candidate = match spec {
            SourceControlProviderDiscoverySpec::ManagedCli(spec) => {
                spec.probe.refine_unknown_remote(cwd, &context).await
            }
            SourceControlProviderDiscoverySpec::Api(_) => None,
            SourceControlProviderDiscoverySpec::Cli(spec) => {
                let Some(refine) = &spec.refine_unknown_remote else {
                    continue;
                };
                let args = spec
                    .remote_refinement_args
                    .as_ref()
                    .unwrap_or(&spec.auth_args);
                match host
                    .process_run(cli_probe_input(
                        spec,
                        cwd,
                        "source-control.discovery.refine-unknown-remote",
                        args,
                        true,
                    ))
                    .await
                {
                    Ok(auth) => refine(&SourceControlUnknownRemoteRefinementInput {
                        cwd: cwd.to_owned(),
                        context: context.clone(),
                        auth: SourceControlAuthProbeInput {
                            stdout: auth.stdout,
                            stderr: auth.stderr,
                            exit_code: auth.exit_code,
                        },
                    }),
                    Err(_) => None,
                }
            }
        };
        if provider.is_none() {
            provider = candidate;
        }
    }
    Some(match provider {
        Some(provider) => SourceControlProviderContext {
            provider,
            ..context
        },
        None => context,
    })
}

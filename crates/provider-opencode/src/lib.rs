//! OpenCode transport and provider adapter.
//!
//! Nested OpenCode documents (sessions, messages, models) stay `serde_json::Value`
//! inside the exact response envelopes. Fields the adapter reads are named by
//! the typed structs in `adapter`.

// `ProviderAdapterV2Error` is provider-core's error type and cannot be boxed here.
#[allow(clippy::result_large_err)]
mod adapter;
mod auth;
mod command;
mod error;
mod events;
mod http_client;
mod ledger;
mod permission_rules;
mod process_group;
mod runtime;
mod semver;
mod server_owner;
mod sse;
mod urlutil;
mod v1_client;
mod v2_client;
mod v2_server;
mod version_probe;

#[cfg(test)]
mod live;
#[cfg(test)]
mod test_support;

pub use adapter::{
    OpenCodeAdapter, OpenCodeSettings, OpenCodeV1Session, OpenCodeV2Session, ProviderAdapterV2,
    ProviderAdapterV2Error, ProviderAdapterV2Event, ProviderSessionRuntime, approves,
    commands_from_list, format_probe_error, models_from_provider_list,
    open_code_child_permission_rules, pending_status, permission_rule_equals, select_generation,
    session_grant_label, shared_provider_session_id, skills_from_list, v1_capabilities,
    v1_rules_as_session, v2_capabilities,
};
pub use auth::{OPENCODE_USERNAME, basic_authorization, encode_uri_component};
pub use command::{SystemOpenCodeBinary, run_open_code_command};
pub use error::{
    EventStreamError, OpenCodeRuntimeError, SilentStreamError, open_code_runtime_error_detail,
};
pub use events::{V1Event, V2Event, V2StreamEvent, decode_v1_event, decode_v2_event};
pub use ledger::{OpenCodeServerLedger, ServerLease};
pub use permission_rules::{
    SessionPermissionRule, agent_path_allows, last_effect, rule, session_permission_rules,
};
pub use runtime::{
    ConnectOpenCodeServer, OpenCodeServerConnection, OpenCodeServerProcess, StartOpenCodeServer,
    VerifyFn, VerifyInput, connect_to_open_code_server, default_v1_verify,
    parse_server_url_from_output, resolve_open_code_config_content,
    resolve_open_code_server_password, start_open_code_server_process, v1_client_password,
    verify_open_code_server_version,
};
pub use semver::{compare_semver_versions, parse_semver};
pub use server_owner::{OpenCodeServerOwner, OwnerConfig};
pub use sse::{EventStream, SILENT_STREAM_TIMEOUT, SseFrame, decode_sse};
pub use v1_client::{Health, OPENCODE_HEALTH_TIMEOUT, PermissionReply, QuestionReply, V1Client};
pub use v2_client::{
    Cursor, DataEnvelope, Interrupt, Located, MessageQuery, Page, ServerInfo, ServerPaths,
    V2Client, paginate_messages,
};
pub use v2_server::{
    OpenCode2Config, OpenCode2Connection, OpenCode2Server, generate_password, server_environment,
    verify_server,
};
pub use version_probe::{
    MINIMUM_OPENCODE_VERSION, OPENCODE_SERVER_PROBE_TIMEOUT, OPENCODE_VERSION_PROBE_TIMEOUT,
    OpenCodeBinary, OpenCodeCommandResult, OpenCodeGeneration, OpenCodeRuntimeProbe, ProbeSettings,
    ProbedOpenCode, classify_opencode_cli_version, probe_open_code_runtime,
};

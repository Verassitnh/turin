//! OpenCode transport: version probe, both server generations, and their HTTP/SSE clients.
//!
//! The orchestration adapter is a later task. Wire types that belong to
//! `turin-contracts` are defined in this crate until that crate catches up.
//! Nested OpenCode documents (sessions, messages, models) stay `serde_json::Value`
//! inside the exact response envelopes.

mod auth;
mod error;
mod events;
mod http_client;
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

pub use auth::{OPENCODE_USERNAME, basic_authorization, encode_uri_component};
pub use error::{
    EventStreamError, OpenCodeRuntimeError, SilentStreamError, open_code_runtime_error_detail,
};
pub use events::{V1Event, V2Event, V2StreamEvent, decode_v1_event, decode_v2_event};
pub use runtime::{
    ConnectOpenCodeServer, OpenCodeServerConnection, OpenCodeServerProcess, StartOpenCodeServer,
    VerifyFn, VerifyInput, connect_to_open_code_server, default_v1_verify,
    parse_server_url_from_output, resolve_open_code_config_content,
    resolve_open_code_server_password, start_open_code_server_process,
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

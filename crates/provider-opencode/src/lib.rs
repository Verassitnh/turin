//! OpenCode transport: version probe, both server generations, and their HTTP/SSE clients.
//!
//! The orchestration adapter is a later task. Wire types that belong to
//! `turin-contracts` are defined in this crate until that crate catches up.

mod auth;
mod error;
mod semver;
mod urlutil;
mod version_probe;

pub use auth::{OPENCODE_USERNAME, basic_authorization, encode_uri_component};
pub use error::{
    EventStreamError, OpenCodeRuntimeError, SilentStreamError, open_code_runtime_error_detail,
};
pub use semver::{compare_semver_versions, parse_semver};
pub use version_probe::{
    MINIMUM_OPENCODE_VERSION, OPENCODE_SERVER_PROBE_TIMEOUT, OPENCODE_VERSION_PROBE_TIMEOUT,
    OpenCodeBinary, OpenCodeCommandResult, OpenCodeGeneration, OpenCodeRuntimeProbe, ProbeSettings,
    ProbedOpenCode, classify_opencode_cli_version, probe_open_code_runtime,
};

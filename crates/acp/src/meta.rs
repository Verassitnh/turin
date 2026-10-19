//! Port of upstream/packages/effect-acp/src/_generated/meta.gen.ts and the v1 method table in rpc.ts.
//!
//! Method names are copied from the pinned `meta-v2.0.0-alpha.3.json` and
//! `meta-v1.21.0.json` releases. The v1 compatibility names that are not in
//! that meta file (`session/set_model`, `session/elicitation`, `_session/elicitation`,
//! `session/elicitation/complete`) match `rpc.ts`.
//!
//! Port of upstream/packages/effect-acp/src/rpc.ts (v1 method table).

pub const V2_SCHEMA_RELEASE: &str = "schema-v2.0.0-alpha.3";
pub const V1_SCHEMA_RELEASE: &str = "schema-v1.21.0";
pub const PROTOCOL_VERSION: i64 = 2;

pub const RPC_ID_START: i64 = 1 << 32;

pub mod agent {
    pub const INITIALIZE: &str = "initialize";
    pub const AUTH_LOGIN: &str = "auth/login";
    pub const AUTH_LOGOUT: &str = "auth/logout";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_LIST: &str = "session/list";
    pub const SESSION_FORK: &str = "session/fork";
    pub const SESSION_RESUME: &str = "session/resume";
    pub const SESSION_CLOSE: &str = "session/close";
    pub const SESSION_DELETE: &str = "session/delete";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_SET_CONFIG_OPTION: &str = "session/set_config_option";
    pub const PROVIDERS_LIST: &str = "providers/list";
    pub const PROVIDERS_SET: &str = "providers/set";
    pub const PROVIDERS_DISABLE: &str = "providers/disable";
    pub const MCP_MESSAGE: &str = "mcp/message";
}

pub mod client {
    pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
    pub const SESSION_UPDATE: &str = "session/update";
    pub const ELICITATION_CREATE: &str = "elicitation/create";
    pub const ELICITATION_COMPLETE: &str = "elicitation/complete";
    pub const MCP_CONNECT: &str = "mcp/connect";
    pub const MCP_MESSAGE: &str = "mcp/message";
    pub const MCP_DISCONNECT: &str = "mcp/disconnect";
}

pub mod v1_agent {
    pub const AUTHENTICATE: &str = "authenticate";
    pub const LOGOUT: &str = "logout";
    pub const SESSION_LOAD: &str = "session/load";
    pub const SESSION_SET_MODEL: &str = "session/set_model";
    pub const SESSION_SET_MODE: &str = "session/set_mode";
}

pub mod v1_client {
    pub const SESSION_ELICITATION: &str = "session/elicitation";
    pub const UNSTABLE_SESSION_ELICITATION: &str = "_session/elicitation";
    pub const SESSION_ELICITATION_COMPLETE: &str = "session/elicitation/complete";
    pub const FS_READ_TEXT_FILE: &str = "fs/read_text_file";
    pub const FS_WRITE_TEXT_FILE: &str = "fs/write_text_file";
    pub const TERMINAL_CREATE: &str = "terminal/create";
    pub const TERMINAL_OUTPUT: &str = "terminal/output";
    pub const TERMINAL_RELEASE: &str = "terminal/release";
    pub const TERMINAL_WAIT_FOR_EXIT: &str = "terminal/wait_for_exit";
    pub const TERMINAL_KILL: &str = "terminal/kill";
}

pub fn client_core_methods() -> &'static [&'static str] {
    &[
        client::SESSION_REQUEST_PERMISSION,
        client::ELICITATION_CREATE,
        v1_client::SESSION_ELICITATION,
        v1_client::UNSTABLE_SESSION_ELICITATION,
        client::MCP_CONNECT,
        client::MCP_MESSAGE,
        client::MCP_DISCONNECT,
        v1_client::FS_READ_TEXT_FILE,
        v1_client::FS_WRITE_TEXT_FILE,
        v1_client::TERMINAL_CREATE,
        v1_client::TERMINAL_OUTPUT,
        v1_client::TERMINAL_RELEASE,
        v1_client::TERMINAL_WAIT_FOR_EXIT,
        v1_client::TERMINAL_KILL,
    ]
}

pub fn agent_core_methods() -> &'static [&'static str] {
    &[
        agent::INITIALIZE,
        agent::AUTH_LOGIN,
        agent::AUTH_LOGOUT,
        agent::SESSION_NEW,
        agent::SESSION_LIST,
        agent::SESSION_FORK,
        agent::SESSION_RESUME,
        agent::SESSION_CLOSE,
        agent::SESSION_DELETE,
        agent::PROVIDERS_LIST,
        agent::PROVIDERS_SET,
        agent::PROVIDERS_DISABLE,
        agent::SESSION_PROMPT,
        agent::SESSION_SET_CONFIG_OPTION,
    ]
}

pub fn is_client_core_method(method: &str) -> bool {
    client_core_methods().contains(&method)
}

pub fn is_agent_core_method(method: &str) -> bool {
    agent_core_methods().contains(&method)
}

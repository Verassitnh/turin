//! Cursor provider over `cursor-agent acp`.
//!
//! Upstream runs `@cursor/sdk` in-process. Turin speaks ACP instead: one
//! `cursor-agent acp` process per thread, explicit `cursor_login`, and T3
//! cards for permissions, questions, and plans. See PORTING.md.

mod account_usage;
mod acp_config;
mod auth;
mod capabilities;
mod driver;
mod error;
mod events;
mod extensions;
mod models;
mod permissions;
mod probe;
mod session;
mod settings;

pub use account_usage::{
    AccountUsageRead, CredentialSource, DashboardClient, DashboardRequest, DashboardResponse,
    KeychainOutcome, ReqwestDashboard, UsageRecord, UsageSpeed, cursor_rate_model,
    read_cursor_account_usage,
};
pub use acp_config::{
    default_cursor_home, restore_user_config, snapshot_user_config, with_user_config,
};
pub use auth::{
    ACP_ARGS, AUTHENTICATE_ON_AUTH_REQUIRED, CURSOR_LOGIN_METHOD, CredentialFileStatus,
    CursorSignInStatus, NOT_SIGNED_IN_MESSAGE, SignInPhase, authenticate_params,
    credential_file_status, is_auth_required_error, is_auth_required_message, launch_is_unforced,
    require_cursor_login_method, status_from_auth_methods, status_from_credential_file,
};
pub use capabilities::{
    SelectionTransition, cursor_capabilities, image_prompts_supported, negotiate_capabilities,
    plan_selection_transition,
};
pub use driver::CursorDriver;
pub use error::{CursorError, CursorOperation};
pub use events::{CursorUpdate, PlanStep, parse_session_update};
pub use extensions::{
    AskDisposition, PlanDisposition, ask_disposition_from_answers, ask_result, plan_result,
    plan_steps, user_input_questions,
};
pub use models::{ConfigUpdate, MODEL_CONFIG_ID, config_updates};
pub use permissions::{
    ALLOW_FOR_THREAD_LABEL, ALWAYS_ALLOW_LABEL, PermissionAnswer, PermissionRequest,
    answer_for_decision, approval_options, parse_permission_request, permission_result,
};
pub use probe::probe_initialize;
pub use session::{
    CursorAdapter, CursorEvent, CursorSession, CursorThread, PromptImage, PromptOutcome,
    RuntimeReply,
};
pub use settings::CursorSettings;

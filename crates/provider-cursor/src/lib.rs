//! Cursor provider over `cursor-agent acp`.
//!
//! Upstream runs `@cursor/sdk` in-process. Turin speaks ACP instead: one
//! `cursor-agent acp` process per thread, explicit `cursor_login`, and T3
//! cards for permissions, questions, and plans. See PORTING.md.

mod account_cache;
mod account_usage;
mod acp_config;
mod auth;
mod capabilities;
mod credential_store;
mod cursor_account_reader;
mod cursor_keychain;
mod cursor_usage_accounts;
mod driver;
mod error;
mod events;
mod extensions;
mod models;
mod permissions;
mod probe;
mod session;
mod settings;
mod sign_in;
mod skills;
mod status;
mod usage_host;
mod usage_limits;

pub use account_cache::{
    CURSOR_ACCOUNT_CACHE_FILE_NAME, CURSOR_ACCOUNT_TTL_MS, CursorAccountCache,
    CursorCredentialSource, cursor_fetch_range, decode_cursor_account_caches,
    encode_cursor_account_caches, is_cursor_cache_fresh, merge_cursor_fetch,
};
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
pub use credential_store::{
    CursorCredentialStoreBundle, CursorSdkCredentialStore, CursorSdkCredentials,
    make_cursor_credential_store,
};
pub use cursor_account_reader::CursorAccountReader;
pub use cursor_keychain::{
    CursorKeychain, CursorKeychainAccessError, CursorKeychainBackend, CursorKeychainReadError,
    CursorKeychainTimeoutError, MacOsSecurityCliKeychain, is_cursor_keychain_timeout_error,
};
pub use cursor_usage_accounts::{
    CursorPlatform, CursorUsageAccounts, CursorUsageEnvironment, CursorUsageScanInput,
    cursor_auth_path,
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
    CursorAdapter, CursorSession, CursorThread, PromptImage, PromptOutcome, RuntimeReply,
};
pub use settings::CursorSettings;
pub use skills::{
    CursorSkillsProbeError, discover_cursor_skills, has_cursor_skill_mention, probe_cursor_skills,
    rewrite_cursor_skill_mentions,
};
pub use usage_host::{
    CursorUsageLimitsSource, HostCursorUsageLimits, cursor_usage_reader, cursor_usage_reader_with,
};
pub use usage_limits::{
    BillingCycleEnd, CursorPlanUsage, CursorUsageHttp, CursorUsageLimitsHost, CursorUsageResponse,
    ReqwestCursorUsageHttp, cursor_usage_response_to_limits, read_cursor_usage_limits,
};

//! Shared provider services and adapter interfaces. See `PORTING.md`.

pub mod adapter_driver;
pub mod attachment_prompt;
pub mod auth;
pub mod background_scope;
pub mod collect_stream_text;
pub mod command_path;
pub mod credential_store;
pub mod driver;
pub mod errors;
pub mod failure;
pub mod handoff_budget;
pub mod host_process;
pub mod id_allocator;
pub mod instance_environment;
pub mod instance_identity;
pub mod js;
pub mod maintenance;
pub mod maintenance_resolver;
pub mod mcp_provider_sessions;
pub mod mcp_session;
pub mod native_protocol_logging;
pub mod notification;
pub mod orchestration_instructions;
pub mod path_expansion;
pub mod process_group;
pub mod provider_adapter;
pub mod provider_adapter_registry;
pub mod provider_continuation_requests;
pub mod provider_errors;
pub mod provider_event_loggers;
pub mod provider_host;
pub mod provider_instance_registry;
pub mod provider_instance_registry_hydration;
pub mod provider_latest_versions;
pub mod rt;
pub mod runtime_instructions;
pub mod selection_transition;
pub mod semver;
pub mod server_provider;
pub mod snapshot;
pub mod snapshot_probe;
pub mod text_generation;
pub mod turn_item;
pub mod unavailable_provider_snapshot;
pub mod usage;
pub mod usage_limits;

pub use adapter_driver::{ProviderAdapterDriver, ProviderAdapterDriverCreateError};
pub use driver::{
    ProviderContinuationIdentity, ProviderDriver, ProviderDriverMetadata, ProviderInstance,
    ProviderInstanceScope, default_provider_continuation_identity,
};
pub use provider_adapter::{ProviderAdapterV2, ProviderAdapterV2Error, ProviderSessionRuntime};
pub use provider_adapter_registry::ProviderAdapterRegistry;
pub use provider_instance_registry::ProviderInstanceRegistry;

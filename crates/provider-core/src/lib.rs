//! Shared provider services and adapter interfaces. See `PORTING.md`.

pub mod attachment_prompt;
pub mod collect_stream_text;
pub mod handoff_budget;
pub mod id_allocator;
pub mod instance_environment;
pub mod js;
pub mod native_protocol_logging;
pub mod notification;
pub mod orchestration_instructions;
pub mod path_expansion;
pub mod process_group;
pub mod provider_latest_versions;
pub mod runtime_instructions;
pub mod turn_item;

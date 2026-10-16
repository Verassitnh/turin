//! Provider wiring that needs more than one provider crate or the runtime.
//! `provider-core` must not depend on the drivers or the runtime, so the pieces
//! that do live here. See `PORTING.md` (R1).

pub mod built_in_drivers;
pub mod project_settings_override_sweep;
pub mod provider_orchestration_adapter_infrastructure;
pub mod provider_registry;

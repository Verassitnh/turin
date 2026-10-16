//! Ports of upstream/apps/web/src/onboarding: the first-run decision, the
//! provider readiness rules and the project import picker logic. Views live in
//! `components/onboarding`.
//!
//! Not ported: `targetEnvironment.logic.ts` (it chooses among paired computers
//! and T3 Connect relays for the cut Connect step) and `useProjectScans.ts`
//! (an Effect atom subscription; the wizard's scan seam replaces it).

// The hosted-static decision is ported (and tested) with the rest of firstRun.logic.
#![allow(dead_code)]

pub mod agent_session_live;
pub mod first_run_logic;
pub mod live_evidence;
pub mod live_providers;
pub mod project_import_logic;
pub mod provider_readiness_logic;

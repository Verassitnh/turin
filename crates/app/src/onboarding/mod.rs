//! Ports of upstream/apps/web/src/onboarding: the first-run decision, the
//! provider readiness rules and the project import picker logic. Views live in
//! `components/onboarding`.
//!
//! Not ported: `targetEnvironment.logic.ts` (it chooses among paired computers
//! and T3 Connect relays for the cut Connect step) and `useProjectScans.ts`
//! (an Effect atom subscription; the wizard's scan seam replaces it).

pub mod first_run_logic;
pub mod project_import_logic;
pub mod provider_readiness_logic;

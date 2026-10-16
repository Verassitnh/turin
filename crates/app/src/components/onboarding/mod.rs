//! Ports of upstream/apps/web/src/components/onboarding.

// The seams carry every field upstream reads; the live wiring (T22) uses the rest.
#![allow(dead_code)]

pub mod backend;
pub mod first_run_gate;
pub mod live_source;
pub mod mock_source;
pub mod welcome_wizard;

//! Turin app runtime: settings, secrets, keybindings, data-dir config, the
//! local environment descriptor and storage cleanup (upstream
//! `apps/server/src` N1 group). The GPUI app calls these services directly.

pub mod config;
pub mod support;

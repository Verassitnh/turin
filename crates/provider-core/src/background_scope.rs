//! `BackgroundScope` from upstream `packages/contracts/src/background.ts`.
//!
//! The background-activity feature is cut. ProviderHost still asks whether a
//! scope should run, so drivers keep the same call. The live host answers
//! `true` until a host policy exists.

use turin_contracts::{ThreadId, provider_instance::ProviderInstanceId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundScope {
    ServerConfig,
    ProviderStatus {
        instance_id: Option<ProviderInstanceId>,
    },
    VcsStatus {
        cwd: String,
    },
    GitRefs {
        cwd: String,
    },
    Diagnostics,
    Thread {
        thread_id: ThreadId,
    },
}

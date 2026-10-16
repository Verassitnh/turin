#![allow(dead_code)]

//! Port of upstream/apps/web/src/hooks/useActiveThreadRef.ts.

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::{EnvironmentId, ThreadId};

/// `useActiveThreadRef` without React: stable scoped ref from ids.
pub fn active_thread_ref(
    environment_id: Option<&EnvironmentId>,
    thread_id: Option<&ThreadId>,
) -> Option<ScopedThreadRef> {
    Some(ScopedThreadRef {
        environment_id: environment_id.cloned()?,
        thread_id: thread_id.cloned()?,
    })
}

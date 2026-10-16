//! Port of upstream/apps/web/src/hooks/useSupportsMultiplePullRequests.ts

#![allow(dead_code)]

use turin_contracts::EnvironmentId;
use turin_contracts::environment::ExecutionEnvironmentCapabilities;

pub fn supports_multiple_pull_requests(
    environment_id: Option<&EnvironmentId>,
    capabilities: Option<&ExecutionEnvironmentCapabilities>,
) -> bool {
    environment_id.is_some()
        && capabilities.is_some_and(|caps| caps.thread_pull_requests == Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::EnvironmentId;

    #[test]
    fn reads_thread_pull_requests_capability() {
        let id = EnvironmentId::new("env").unwrap();
        let caps = ExecutionEnvironmentCapabilities {
            thread_pull_requests: Some(true),
            ..Default::default()
        };
        assert!(supports_multiple_pull_requests(Some(&id), Some(&caps)));
        assert!(!supports_multiple_pull_requests(None, Some(&caps)));
        assert!(!supports_multiple_pull_requests(
            Some(&id),
            Some(&ExecutionEnvironmentCapabilities::default())
        ));
    }
}

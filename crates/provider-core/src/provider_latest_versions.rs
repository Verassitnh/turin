//! Port of upstream/packages/provider-core/src/server/ProviderLatestVersions.ts.

use parking_lot::Mutex;
use std::{
    collections::HashMap,
    future::Future,
    time::{Duration, Instant},
};

const FOUND_TTL: Duration = Duration::from_secs(60 * 60);
const MISSING_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct Entry {
    version: Option<String>,
    expires_at: Option<Instant>,
}

/// Shared memo for installer or registry version probes. A failed lookup is
/// not cached; an absent version is cached for one minute.
#[derive(Debug, Default)]
pub struct ProviderLatestVersions {
    entries: Mutex<HashMap<String, Entry>>,
}

impl ProviderLatestVersions {
    pub fn new(seed: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        Self {
            entries: Mutex::new(
                seed.into_iter()
                    .map(|(key, version)| {
                        (
                            key,
                            Entry {
                                version,
                                expires_at: None,
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }

    pub async fn cached<E, F, Fut>(
        &self,
        key: &str,
        lookup: F,
        fresh: bool,
    ) -> Result<Option<String>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<String>, E>>,
    {
        // `now` is read before the lookup, as upstream does, so the TTL
        // counts from when the lookup started.
        let now = Instant::now();
        if !fresh
            && let Some(entry) = self.entries.lock().get(key)
            && entry.expires_at.is_none_or(|at| at > now)
        {
            return Ok(entry.version.clone());
        }
        let version = lookup().await?;
        let ttl = if version.is_some() {
            FOUND_TTL
        } else {
            MISSING_TTL
        };
        self.entries.lock().insert(
            key.into(),
            Entry {
                version: version.clone(),
                expires_at: Some(now + ttl),
            },
        );
        Ok(version)
    }

    pub fn invalidate(&self, key: &str) {
        self.entries.lock().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn seed_refresh_and_invalidate() {
        let versions = ProviderLatestVersions::new([("codex".into(), Some("1.0".into()))]);
        let cached = versions
            .cached("codex", || async { Ok::<_, ()>(Some("2.0".into())) }, false)
            .await
            .unwrap();
        assert_eq!(cached.as_deref(), Some("1.0"));
        let fresh = versions
            .cached("codex", || async { Ok::<_, ()>(Some("2.0".into())) }, true)
            .await
            .unwrap();
        assert_eq!(fresh.as_deref(), Some("2.0"));
        versions.invalidate("codex");
        let looked_up = versions
            .cached("codex", || async { Ok::<_, ()>(None) }, false)
            .await
            .unwrap();
        assert_eq!(looked_up, None);
    }
}

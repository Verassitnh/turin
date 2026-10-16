//! Port of upstream/packages/client-runtime/src/projectFaviconCache.ts
//! (in-memory cache; web IndexedDB seam is `ProjectFaviconStorage`).

#![allow(dead_code)]

use std::collections::HashMap;

use turin_contracts::EnvironmentId;
use turin_shared::project_favicon::{
    get_project_favicon_cache_key, get_project_favicon_resource_key,
    is_project_favicon_fallback_url,
};

pub const PROJECT_FAVICON_THUMBNAIL_SIZE: u32 = 96;
pub const PROJECT_FAVICON_MAX_DATA_URL_LENGTH: usize = 32 * 1024;
pub const PROJECT_FAVICON_MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
pub const PROJECT_FAVICON_CACHE_MAX_BYTES: usize = 1024 * 1024;
pub const PROJECT_FAVICON_CACHE_MAX_ENTRIES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFaviconTarget {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub favicon_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFaviconEntry {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub favicon_path: Option<String>,
    pub revision: String,
    pub data_url: String,
}

fn resource_key(target: &ProjectFaviconTarget) -> String {
    get_project_favicon_resource_key(
        target.environment_id.as_str(),
        &target.cwd,
        target.favicon_path.as_deref(),
    )
}

pub trait ProjectFaviconStorage: Send + Sync {
    fn list(&self) -> Vec<ProjectFaviconEntry>;
    fn put(&self, key: &str, entry: &ProjectFaviconEntry);
    fn remove(&self, key: &str, entry: &ProjectFaviconEntry);
}

#[derive(Default)]
pub struct ProjectFaviconCache {
    entries: HashMap<String, ProjectFaviconEntry>,
    environment_revisions: HashMap<String, u32>,
    generation: u32,
}

impl ProjectFaviconCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn hydrate(&mut self, storage: &dyn ProjectFaviconStorage) {
        for entry in storage.list() {
            if entry.data_url.len() <= PROJECT_FAVICON_MAX_DATA_URL_LENGTH
                && entry.data_url.starts_with("data:image/")
            {
                self.entries.insert(
                    resource_key(&ProjectFaviconTarget {
                        environment_id: entry.environment_id.clone(),
                        cwd: entry.cwd.clone(),
                        favicon_path: entry.favicon_path.clone(),
                    }),
                    entry,
                );
            }
        }
        self.trim();
    }

    pub fn peek(&self, target: &ProjectFaviconTarget) -> Option<&str> {
        self.entries
            .get(&resource_key(target))
            .map(|entry| entry.data_url.as_str())
    }

    pub fn insert(
        &mut self,
        target: ProjectFaviconTarget,
        url: &str,
        data_url: String,
        storage: Option<&dyn ProjectFaviconStorage>,
    ) {
        if is_project_favicon_fallback_url(Some(url)) {
            self.remove(&target, storage);
            return;
        }
        if data_url.len() > PROJECT_FAVICON_MAX_DATA_URL_LENGTH {
            return;
        }
        let revision =
            get_project_favicon_cache_key(target.environment_id.as_str(), &target.cwd, url);
        let key = resource_key(&target);
        let entry = ProjectFaviconEntry {
            environment_id: target.environment_id,
            cwd: target.cwd,
            favicon_path: target.favicon_path,
            revision,
            data_url: data_url.clone(),
        };
        self.entries.insert(key.clone(), entry.clone());
        if let Some(storage) = storage {
            storage.put(&key, &entry);
        }
        self.trim();
    }

    pub fn remove(
        &mut self,
        target: &ProjectFaviconTarget,
        storage: Option<&dyn ProjectFaviconStorage>,
    ) {
        let key = resource_key(target);
        if let Some(entry) = self.entries.remove(&key)
            && let Some(storage) = storage
        {
            storage.remove(&key, &entry);
        }
    }

    pub fn clear_environment(&mut self, environment_id: &EnvironmentId) {
        let revision = self
            .environment_revisions
            .entry(environment_id.as_str().to_owned())
            .or_insert(0);
        *revision += 1;
        self.generation += 1;
        self.entries
            .retain(|_, entry| entry.environment_id != *environment_id);
    }

    fn trim(&mut self) {
        let mut bytes: usize = self.entries.values().map(|e| e.data_url.len()).sum();
        while self.entries.len() > PROJECT_FAVICON_CACHE_MAX_ENTRIES
            || bytes > PROJECT_FAVICON_CACHE_MAX_BYTES
        {
            let Some(oldest_key) = self.entries.keys().next().cloned() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest_key) {
                bytes = bytes.saturating_sub(entry.data_url.len());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_small_data_urls_and_peeks_them() {
        let mut cache = ProjectFaviconCache::new();
        let target = ProjectFaviconTarget {
            environment_id: EnvironmentId::new("env").unwrap(),
            cwd: "/repo".into(),
            favicon_path: None,
        };
        cache.insert(
            target.clone(),
            "https://example.com/favicon.ico",
            "data:image/png;base64,AA==".into(),
            None,
        );
        assert_eq!(cache.peek(&target), Some("data:image/png;base64,AA=="));
    }

    #[test]
    fn removes_entries_for_fallback_marker_urls() {
        let mut cache = ProjectFaviconCache::new();
        let target = ProjectFaviconTarget {
            environment_id: EnvironmentId::new("env").unwrap(),
            cwd: "/repo".into(),
            favicon_path: None,
        };
        cache.insert(
            target.clone(),
            "https://example.com/assets/project-favicon-missing",
            "data:image/png;base64,AA==".into(),
            None,
        );
        assert!(cache.peek(&target).is_none());
    }
}

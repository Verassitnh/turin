//! Port of upstream/apps/web/src/components/chat/folderDrop.ts

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderDropTarget {
    Local,
    Remote,
}

pub fn folder_drop_target(
    local_environment_disabled: bool,
    environment_id: &str,
    primary_environment_id: Option<&str>,
) -> FolderDropTarget {
    if local_environment_disabled
        || primary_environment_id.is_none()
        || primary_environment_id != Some(environment_id)
    {
        FolderDropTarget::Remote
    } else {
        FolderDropTarget::Local
    }
}

pub fn resolve_dropped_folder_path(path: Option<&str>) -> Option<&str> {
    match path {
        Some(path) if !path.is_empty() => Some(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_local_when_the_thread_is_on_the_primary_environment() {
        assert_eq!(
            folder_drop_target(false, "environment-1", Some("environment-1")),
            FolderDropTarget::Local
        );
    }

    #[test]
    fn targets_remote_when_there_is_no_local_environment() {
        assert_eq!(
            folder_drop_target(true, "environment-1", Some("environment-1")),
            FolderDropTarget::Remote
        );
    }

    #[test]
    fn targets_remote_when_the_thread_lives_on_another_environment() {
        assert_eq!(
            folder_drop_target(false, "environment-2", Some("environment-1")),
            FolderDropTarget::Remote
        );
    }

    #[test]
    fn targets_remote_when_no_primary_environment_is_known() {
        assert_eq!(
            folder_drop_target(false, "environment-1", None),
            FolderDropTarget::Remote
        );
    }

    #[test]
    fn returns_the_native_path_when_the_bridge_provides_it() {
        assert_eq!(
            resolve_dropped_folder_path(Some("/tmp/project/contracts")),
            Some("/tmp/project/contracts")
        );
    }

    #[test]
    fn returns_null_when_the_bridge_is_missing_or_the_path_is_empty() {
        assert_eq!(resolve_dropped_folder_path(None), None);
        assert_eq!(resolve_dropped_folder_path(Some("")), None);
    }
}

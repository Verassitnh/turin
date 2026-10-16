//! Port of upstream/apps/web/src/components/chat/workspaceFileDrop.ts

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedFile {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragItem {
    pub kind: String,
    pub file: Option<DroppedFile>,
    pub is_directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataTransfer {
    pub types: Vec<String>,
    pub files: Vec<DroppedFile>,
    pub items: Option<Vec<DragItem>>,
    pub drop_effect: String,
}

pub struct WorkspaceFileDragEvent {
    pub data_transfer: DataTransfer,
    pub moved_within_target: bool,
    pub default_prevented: bool,
}

pub trait WorkspaceFileDropHost {
    fn set_drag_active(&mut self, active: bool);
    fn add_files(&mut self, files: &[DroppedFile]);
    fn add_folders(&mut self, folders: &[DroppedFile]);
}

fn is_file_drag(event: &WorkspaceFileDragEvent) -> bool {
    event.data_transfer.types.iter().any(|kind| kind == "Files")
}

pub fn on_drag_enter(host: &mut impl WorkspaceFileDropHost, event: &mut WorkspaceFileDragEvent) {
    if !is_file_drag(event) {
        return;
    }
    event.default_prevented = true;
    if event.moved_within_target {
        return;
    }
    host.set_drag_active(true);
}

pub fn on_drag_over(host: &mut impl WorkspaceFileDropHost, event: &mut WorkspaceFileDragEvent) {
    if !is_file_drag(event) {
        return;
    }
    event.default_prevented = true;
    event.data_transfer.drop_effect = "copy".to_owned();
    host.set_drag_active(true);
}

pub fn on_drag_leave(host: &mut impl WorkspaceFileDropHost, event: &mut WorkspaceFileDragEvent) {
    if !is_file_drag(event) {
        return;
    }
    event.default_prevented = true;
    if event.moved_within_target {
        return;
    }
    host.set_drag_active(false);
}

pub fn on_drop(host: &mut impl WorkspaceFileDropHost, event: &mut WorkspaceFileDragEvent) {
    if !is_file_drag(event) {
        return;
    }
    event.default_prevented = true;
    host.set_drag_active(false);
    let (files, folders) = split_dropped_items(&event.data_transfer);
    if !files.is_empty() {
        host.add_files(&files);
    }
    if !folders.is_empty() {
        host.add_folders(&folders);
    }
}

fn split_dropped_items(data_transfer: &DataTransfer) -> (Vec<DroppedFile>, Vec<DroppedFile>) {
    let Some(items) = &data_transfer.items else {
        return (data_transfer.files.clone(), Vec::new());
    };
    let mut files = Vec::new();
    let mut folders = Vec::new();
    for item in items {
        if item.kind != "file" {
            continue;
        }
        let Some(file) = item.file.clone() else {
            continue;
        };
        if item.is_directory {
            folders.push(file);
        } else {
            files.push(file);
        }
    }
    (files, folders)
}

#[cfg(test)]
#[path = "workspace_file_drop_tests.rs"]
mod tests;

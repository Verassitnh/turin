//! Port of upstream/apps/web/src/components/chat/workspaceFileDrop.test.ts

use super::*;

#[derive(Default)]
struct Host {
    active: Vec<bool>,
    files: Vec<Vec<DroppedFile>>,
    folders: Vec<Vec<DroppedFile>>,
}

impl WorkspaceFileDropHost for Host {
    fn set_drag_active(&mut self, active: bool) {
        self.active.push(active);
    }
    fn add_files(&mut self, files: &[DroppedFile]) {
        self.files.push(files.to_vec());
    }
    fn add_folders(&mut self, folders: &[DroppedFile]) {
        self.folders.push(folders.to_vec());
    }
}

fn file(name: &str) -> DroppedFile {
    DroppedFile { name: name.into() }
}

fn event(
    types: &[&str],
    files: Vec<DroppedFile>,
    items: Option<Vec<DragItem>>,
    moved: bool,
) -> WorkspaceFileDragEvent {
    WorkspaceFileDragEvent {
        data_transfer: DataTransfer {
            types: types.iter().map(|kind| (*kind).to_owned()).collect(),
            files,
            items,
            drop_effect: "none".into(),
        },
        moved_within_target: moved,
        default_prevented: false,
    }
}

#[test]
fn activates_the_target_for_an_external_file_drag() {
    let mut host = Host::default();
    let mut event = event(&["Files"], Vec::new(), None, false);
    on_drag_enter(&mut host, &mut event);
    assert!(event.default_prevented);
    assert_eq!(host.active, [true]);
}

#[test]
fn ignores_non_file_drags() {
    let mut host = Host::default();
    let mut event = event(&["text/plain"], Vec::new(), None, false);
    on_drag_over(&mut host, &mut event);
    assert!(!event.default_prevented);
    assert!(host.active.is_empty());
}

#[test]
fn does_not_flicker_when_the_drag_moves_between_children() {
    let mut host = Host::default();
    let mut event = event(&["Files"], Vec::new(), None, true);
    on_drag_enter(&mut host, &mut event);
    on_drag_leave(&mut host, &mut event);
    assert!(host.active.is_empty());
}

#[test]
fn forwards_dropped_files_and_clears_the_active_state() {
    let example = file("example.txt");
    let mut host = Host::default();
    let mut event = event(&["Files"], vec![example.clone()], None, false);
    on_drop(&mut host, &mut event);
    assert_eq!(host.active, [false]);
    assert_eq!(host.files, [vec![example]]);
}

#[test]
fn routes_mixed_drops_to_files_and_folders() {
    let example = file("example.txt");
    let folder = file("project");
    let mut host = Host::default();
    let mut event = event(
        &["Files"],
        Vec::new(),
        Some(vec![
            DragItem {
                kind: "file".into(),
                file: Some(folder.clone()),
                is_directory: true,
            },
            DragItem {
                kind: "file".into(),
                file: Some(example.clone()),
                is_directory: false,
            },
        ]),
        false,
    );
    on_drop(&mut host, &mut event);
    assert_eq!(host.files, [vec![example]]);
    assert_eq!(host.folders, [vec![folder]]);
}

#[test]
fn routes_a_folder_only_drop_without_attaching_files() {
    let folder = file("project");
    let mut host = Host::default();
    let mut event = event(
        &["Files"],
        Vec::new(),
        Some(vec![DragItem {
            kind: "file".into(),
            file: Some(folder.clone()),
            is_directory: true,
        }]),
        false,
    );
    on_drop(&mut host, &mut event);
    assert!(host.files.is_empty());
    assert_eq!(host.folders, [vec![folder]]);
}

#[test]
fn uses_files_when_the_browser_does_not_expose_drag_items() {
    let example = file("example.txt");
    let mut host = Host::default();
    let mut event = event(&["Files"], vec![example.clone()], None, false);
    on_drop(&mut host, &mut event);
    assert_eq!(host.files, [vec![example]]);
    assert!(host.folders.is_empty());
}

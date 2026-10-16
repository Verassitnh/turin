//! Port of upstream/apps/web/src/components/chat/PierreEntryIcon.tsx
//! and upstream/apps/web/src/components/chat/WorkspaceEntryIcon.tsx
//!
//! The file or folder glyph beside a path chip. Upstream draws a sprite symbol (`@pierre/trees`)
//! tinted by the icon's token; Turin has no sprite sheet, so a resolved icon is the neutral file
//! glyph in the token's color, and an entry with no resolved icon is the neutral file or folder
//! glyph, as upstream falls back. The resolution itself (`resolvePierreIconForEntry`) belongs to
//! `pierre-icons.ts`, so the caller hands it in.

use gpui_kit::{App, Hsla, IntoElement, RenderOnce, Window};

use crate::theme::ActiveAppTheme;
use crate::theme::color::hex;
use crate::ui::{Icon, IconName};
use gpui_kit::rems;

/// `ICON_COLORS[token]`: `[light, dark]`.
const ICON_COLORS: &[(&str, [u32; 2])] = &[
    ("astro", [0xa631be, 0xd568ea]),
    ("babel", [0xd5a910, 0xffd452]),
    ("bash", [0x199f43, 0x5ecc71]),
    ("biome", [0x1a85d4, 0x69b1ff]),
    ("bootstrap", [0x693acf, 0x9d6afb]),
    ("browserslist", [0xd5a910, 0xffd452]),
    ("bun", [0x594c5b, 0x79697b]),
    ("c", [0x1a85d4, 0x69b1ff]),
    ("claude", [0xd47628, 0xffa359]),
    ("cpp", [0x1a85d4, 0x69b1ff]),
    ("css", [0x693acf, 0x9d6afb]),
    ("database", [0xa631be, 0xd568ea]),
    ("default", [0x84848a, 0xadadb1]),
    ("docker", [0x1a85d4, 0x69b1ff]),
    ("eslint", [0x693acf, 0x9d6afb]),
    ("git", [0xff8c5b, 0xd5512f]),
    ("go", [0x1ca1c7, 0x68cdf2]),
    ("graphql", [0xd32a61, 0xff678d]),
    ("html", [0xd47628, 0xffa359]),
    ("image", [0xd32a61, 0xff678d]),
    ("javascript", [0xd5a910, 0xffd452]),
    ("json", [0xd47628, 0xffa359]),
    ("markdown", [0x199f43, 0x5ecc71]),
    ("mcp", [0x17a5af, 0x64d1db]),
    ("nextjs", [0x84848a, 0xadadb1]),
    ("npm", [0xd52c36, 0xff6762]),
    ("oxc", [0x1ca1c7, 0x68cdf2]),
    ("postcss", [0xd52c36, 0xff6762]),
    ("prettier", [0x17a5af, 0x64d1db]),
    ("python", [0x1a85d4, 0x69b1ff]),
    ("react", [0x1ca1c7, 0x68cdf2]),
    ("ruby", [0xd52c36, 0xff6762]),
    ("rust", [0xd47628, 0xffa359]),
    ("sass", [0xd32a61, 0xff678d]),
    ("stylelint", [0x84848a, 0xadadb1]),
    ("svelte", [0xd52c36, 0xff6762]),
    ("svg", [0xd47628, 0xffa359]),
    ("svgo", [0x199f43, 0x5ecc71]),
    ("swift", [0xd47628, 0xffa359]),
    ("table", [0x17a5af, 0x64d1db]),
    ("tailwind", [0x1ca1c7, 0x68cdf2]),
    ("terraform", [0x693acf, 0x9d6afb]),
    ("text", [0x84848a, 0xadadb1]),
    ("typescript", [0x1a85d4, 0x69b1ff]),
    ("vite", [0xa631be, 0xd568ea]),
    ("vscode", [0x1a85d4, 0x69b1ff]),
    ("vue", [0x199f43, 0x5ecc71]),
    ("wasm", [0x693acf, 0x9d6afb]),
    ("webpack", [0x1a85d4, 0x69b1ff]),
    ("yml", [0xd52c36, 0xff6762]),
    ("zig", [0xd47628, 0xffa359]),
    ("zip", [0xd47628, 0xffa359]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconTheme {
    Light,
    Dark,
}

/// `PierreIconResolution`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PierreIconResolution {
    pub name: String,
    pub token: Option<String>,
}

/// The tint for a token (`ICON_COLORS[token ?? "default"] ?? ICON_COLORS.default`).
pub fn pierre_icon_color(token: Option<&str>, theme: IconTheme) -> u32 {
    let find = |token: &str| ICON_COLORS.iter().find(|(name, _)| *name == token);
    let (_, colors) = find(token.unwrap_or("default"))
        .or_else(|| find("default"))
        .expect("the table has a default");
    colors[usize::from(theme == IconTheme::Dark)]
}

#[derive(IntoElement)]
pub struct PierreEntryIcon {
    pub kind: EntryKind,
    pub icon: Option<PierreIconResolution>,
    pub theme: IconTheme,
}

impl RenderOnce for PierreEntryIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.app_theme().icon_muted;
        match (&self.icon, self.kind) {
            (Some(icon), EntryKind::File) => {
                let color: Hsla = hex(pierre_icon_color(icon.token.as_deref(), self.theme)).into();
                Icon::new(IconName::FileText).size(rems(1.)).color(color)
            }
            (_, EntryKind::Directory) => Icon::new(IconName::Folder).size(rems(1.)).color(muted),
            (None, EntryKind::File) => Icon::new(IconName::File).size(rems(1.)).color(muted),
        }
    }
}

/// What the file-metadata query knows about a path (`useFileMetadata`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileMetadata {
    pub kind: Option<EntryKind>,
    pub mime_type: Option<String>,
    pub byte_length: Option<u64>,
}

/// `WorkspaceEntryIcon`: a directory only where the metadata says so; a file icon is neutral
/// while the environment is offline or the path is unknown.
pub fn workspace_entry_icon(
    metadata: Option<&FileMetadata>,
    icon: Option<PierreIconResolution>,
    theme: IconTheme,
) -> PierreEntryIcon {
    let kind = if metadata.and_then(|metadata| metadata.kind) == Some(EntryKind::Directory) {
        EntryKind::Directory
    } else {
        EntryKind::File
    };
    PierreEntryIcon { kind, icon, theme }
}

/// `WorkspaceEntryTooltip`: the label (else the path), then the size once it is known.
pub fn workspace_entry_tooltip(
    path: &str,
    label: Option<&str>,
    metadata: Option<&FileMetadata>,
) -> String {
    let text = label.unwrap_or(path);
    match metadata.and_then(|metadata| metadata.byte_length) {
        Some(bytes) => format!(
            "{text} · {}",
            crate::composer::attachment_files::format_attachment_size(bytes)
        ),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tints_by_token_and_theme() {
        assert_eq!(pierre_icon_color(Some("rust"), IconTheme::Light), 0xd47628);
        assert_eq!(pierre_icon_color(Some("rust"), IconTheme::Dark), 0xffa359);
        assert_eq!(pierre_icon_color(Some("git"), IconTheme::Light), 0xff8c5b);
        assert_eq!(pierre_icon_color(Some("git"), IconTheme::Dark), 0xd5512f);
    }

    #[test]
    fn an_unknown_or_missing_token_takes_the_default_color() {
        assert_eq!(pierre_icon_color(None, IconTheme::Light), 0x84848a);
        assert_eq!(
            pierre_icon_color(Some("not-a-token"), IconTheme::Dark),
            0xadadb1
        );
    }

    #[test]
    fn a_workspace_entry_is_a_file_unless_the_metadata_says_directory() {
        let directory = FileMetadata {
            kind: Some(EntryKind::Directory),
            ..Default::default()
        };
        assert_eq!(
            workspace_entry_icon(Some(&directory), None, IconTheme::Light).kind,
            EntryKind::Directory
        );
        assert_eq!(
            workspace_entry_icon(None, None, IconTheme::Light).kind,
            EntryKind::File
        );
        let unknown = FileMetadata::default();
        assert_eq!(
            workspace_entry_icon(Some(&unknown), None, IconTheme::Light).kind,
            EntryKind::File
        );
    }

    #[test]
    fn the_tooltip_names_the_entry_and_its_size_once_known() {
        assert_eq!(
            workspace_entry_tooltip("src/main.rs", None, None),
            "src/main.rs"
        );
        let sized = FileMetadata {
            byte_length: Some(48 * 1024),
            ..Default::default()
        };
        assert_eq!(
            workspace_entry_tooltip("src/main.rs", Some("main.rs"), Some(&sized)),
            "main.rs · 48 KB"
        );
    }
}

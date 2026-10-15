//! Lucide icons the settings pages use on top of the shell's set
//! ([`crate::assets::AppIcons`]); chained into [`crate::assets::AppAssets`].

use gpui_kit::assets::icon_assets;

icon_assets!(
    pub SettingsIcons,
    [
        Archive,
        ArchiveRestore,
        BadgeCheck,
        Bell,
        Blocks,
        CircleX,
        Copy,
        Download,
        Ellipsis,
        ExternalLink,
        Eye,
        EyeOff,
        FileBraces,
        FolderOpen,
        GitPullRequest,
        GripVertical,
        HardDrive,
        Info,
        Keyboard,
        Layers,
        Lock,
        LockOpen,
        PenLine,
        Sparkles,
        ChevronUp,
        ChevronsUpDown,
        Minus,
        Monitor,
        Moon,
        PanelsTopLeft,
        Palette,
        Pencil,
        Play,
        RefreshCw,
        RotateCcw,
        Settings2,
        Star,
        Sun,
        Trash,
        Github,
        TriangleAlert,
        Undo2,
    ]
);

/// Provider marks (components/Icons.tsx): `brand/openai.svg` etc.
pub const BRAND_ICONS: &[(&str, &[u8])] = &[
    ("brand/openai.svg", include_bytes!("../../assets/brand/openai.svg")),
    ("brand/claude.svg", include_bytes!("../../assets/brand/claude.svg")),
    ("brand/cursor.svg", include_bytes!("../../assets/brand/cursor.svg")),
    ("brand/opencode.svg", include_bytes!("../../assets/brand/opencode.svg")),
];

/// The bundled mark for a provider driver kind.
pub fn provider_icon_path(driver: &str) -> Option<&'static str> {
    match driver {
        "codex" => Some("brand/openai.svg"),
        "claudeAgent" => Some("brand/claude.svg"),
        "cursor" => Some("brand/cursor.svg"),
        "opencode" => Some("brand/opencode.svg"),
        _ => None,
    }
}

pub fn load_brand(path: &str) -> Option<&'static [u8]> {
    BRAND_ICONS.iter().find(|(name, _)| *name == path).map(|(_, bytes)| *bytes)
}

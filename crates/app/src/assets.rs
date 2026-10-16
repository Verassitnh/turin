//! Asset source: the lucide icons the UI uses (upstream imports them from
//! `lucide-react`) and the composer's send glyph, falling back to gpui-kit's default component icons.
//!
//! Only the icons listed in [`AppIcons`] are embedded; add one here before
//! using it, otherwise it renders blank.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};

use crate::components::usage::icons::UsageIcons;
use gpui_kit::{AssetSource, Result, SharedString};

icon_assets!(
    pub AppIcons,
    [
        AlarmClock,
        AlarmClockOff,
        Archive,
        ArrowDownUp,
        ArrowLeft,
        ArrowRight,
        ArrowRightLeft,
        ArrowUp,
        Blocks,
        Bookmark,
        ArrowUpRight,
        BookOpen,
        Bot,
        Brain,
        CalendarArrowDown,
        CalendarArrowUp,
        ChartNoAxesColumn,
        Check,
        CheckCheck,
        ChevronDown,
        ChevronLeft,
        ChevronRight,
        ChevronsDownUp,
        ChevronsUpDown,
        ChevronUp,
        CircleAlert,
        CircleCheck,
        CircleDashed,
        CircleGauge,
        CircleX,
        Clock3,
        Circle,
        CircleDot,
        Clock,
        Code,
        Columns2,
        Copy,
        CornerUpRight,
        Ellipsis,
        Eye,
        File,
        CornerDownRight,
        ExternalLink,
        FileCode,
        FileDiff,
        Files,
        FileText,
        Flag,
        Folder,
        FolderOpen,
        FolderCode,
        FolderGit2,
        FolderPlus,
        FolderTree,
        GitBranch,
        GitCommitHorizontal,
        GitFork,
        Github,
        GitMerge,
        GitPullRequestArrow,
        GitPullRequestClosed,
        GitPullRequestDraft,
        Hash,
        Globe,
        Hammer,
        Image,
        Hourglass,
        Info,
        Layers,
        Lightbulb,
        GitPullRequest,
        GripVertical,
        Link,
        Link2,
        ListChecks,
        ListFilter,
        ListOrdered,
        ListPlus,
        Lock,
        LockOpen,
        LoaderCircle,
        Maximize2,
        MessageCircle,
        MessageSquare,
        MessageSquareOff,
        MessageSquareText,
        MessageSquareWarning,
        MessagesSquare,
        MailOpen,
        MessageCircleQuestionMark,
        MessageSquarePlus,
        Minimize2,
        Minus,
        OctagonAlert,
        Package,
        PanelLeft,
        PanelLeftClose,
        PanelRight,
        Paperclip,
        PenLine,
        Pencil,
        PencilRuler,
        Quote,
        Pilcrow,
        Pin,
        Play,
        PinOff,
        Plug2,
        Plus,
        Redo2,
        RefreshCw,
        ShieldQuestionMark,
        Timer,
        RotateCw,
        Reply,
        Rocket,
        RotateCcw,
        Route,
        Rows3,
        Search,
        Send,
        Settings,
        Settings2,
        Shield,
        ShieldCheck,
        Smartphone,
        Sparkles,
        Square,
        SquareCheck,
        SquareMenu,
        SquarePen,
        Star,
        Tag,
        Terminal,
        TextWrap,
        ThumbsUp,
        Trash,
        TriangleAlert,
        Undo2,
        UserRound,
        Wrench,
        Workflow,
        Unlink2,
        UserCheck,
        UserLock,
        UserPlus,
        UserRoundX,
        Users,
        X,
        Zap,
        ArrowDown,
        Cloud,
        CloudDownload,
        CloudUpload,
        Computer,
        CornerLeftUp,
        FileSearch,
        FolderGit,
        GitBranchPlus,
        Laptop,
        MessageSquareDashed,
        Monitor,
        Moon,
        Palette,
        Scale,
        Server,
        Sun,
        TextSearch,
    ]
);

/// The composer's send arrow (ComposerPrimaryActions.tsx).
pub const SEND_ARROW: &str = "brand/send-arrow.svg";
/// Lucide `chevron-down` at `stroke-width` 2.25 (ComposerControl.tsx); the
/// stock `IconName::ChevronDown` is stroke 2.
pub const CHEVRON_DOWN: &str = "brand/chevron-down.svg";
/// EnvironmentMachineIcon's hand-drawn Mac mini / Mac Studio (lucide has no Apple desktops).
pub const MAC_MINI: &str = "brand/mac-mini.svg";
pub const MAC_STUDIO: &str = "brand/mac-studio.svg";
/// The bare T mark for the sidebar brand slot (upstream `T3Wordmark`). Bytes are
/// `assets/prod/wordmark.svg`, `currentColor`. The asset key stays `brand/`.
pub const WORDMARK: &str = "brand/wordmark.svg";
/// The macOS app icon, the same `assets/prod/black-macos-1024.png` that
/// `scripts/bundle-macos.sh` turns into `Turin.icns`. Set as the Dock icon of an
/// unbundled run.
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/prod/black-macos-1024.png");

/// Provider marks (components/Icons.tsx), keyed by driver kind in
/// [`provider_icon_path`].
pub const PROVIDER_OPENAI: &str = "brand/openai.svg";
pub const PROVIDER_CLAUDE: &str = "brand/claude.svg";
pub const PROVIDER_CURSOR: &str = "brand/cursor.svg";

/// The bundled mark for a provider driver kind, if upstream has one inline.
pub fn provider_icon_path(driver_kind: &str) -> Option<&'static str> {
    match driver_kind {
        "codex" => Some(PROVIDER_OPENAI),
        "claudeAgent" => Some(PROVIDER_CLAUDE),
        "cursor" => Some(PROVIDER_CURSOR),
        _ => None,
    }
}

/// `UltrafastIcon` (`currentColor`; the back bolt is 40% opaque), shown by the composer's
/// speed trait for the Ultrafast service tier.
pub const ULTRAFAST: &str = "brand/ultrafast.svg";
/// `LinuxIcon` (`currentColor`), the Linux/WSL machine kind in `EnvironmentMachineIcon`.
pub const LINUX: &str = "brand/linux.svg";
/// `GitIcon`, a fixed-colour mark: draw it with `img`, not the tinted `Icon`.
pub const GIT: &str = "brand/git.svg";
/// `JujutsuIcon`, a multi-colour mark: draw it with `img`, not the tinted `Icon`.
pub const JUJUTSU: &str = "brand/jujutsu.svg";
/// `GitLabIcon`, multi-colour. Draw it with `img`; [`GITLAB_MONOCHROME`] is the
/// `monochrome` variant (every path `currentColor`) for the tinted `Icon::path`.
pub const GITLAB: &str = "brand/gitlab.svg";
/// `GitLabIcon` with `monochrome` set (`ThemeSearchSection` source links).
pub const GITLAB_MONOCHROME: &str = "brand/gitlab-mono.svg";
/// `AzureDevOpsIcon` (selfhst/icons, CC BY 4.0), gradient fills: draw it with `img`.
/// The `useId` gradient ids are fixed (`azure-a`..`azure-i`).
pub const AZURE_DEVOPS: &str = "brand/azure-devops.svg";
/// `BitbucketIcon`, gradient fill (fixed id `bitbucket-a`): draw it with `img`.
pub const BITBUCKET: &str = "brand/bitbucket.svg";
/// `ForgejoIcon`, the official two-colour mark: draw it with `img`.
pub const FORGEJO: &str = "brand/forgejo.svg";
/// `GitCafeIcon`, the inset violet square: draw it with `img`.
pub const GITCAFE: &str = "brand/gitcafe.svg";
/// `ACPRegistryIcon` (`currentColor`; upstream's `fill-black dark:fill-white`
/// becomes the tint the caller sets).
pub const ACP_REGISTRY: &str = "brand/acp-registry.svg";
/// `MacScreenRecordingIcon`, the System Settings pane mark (fixed id `mac-screen-recording-a`).
pub const MAC_SCREEN_RECORDING: &str = "brand/mac-screen-recording.svg";
/// `MacAccessibilityIcon`, the System Settings pane mark (fixed id `mac-accessibility-a`).
pub const MAC_ACCESSIBILITY: &str = "brand/mac-accessibility.svg";

/// `SOURCE_CONTROL_ICONS` (sourceControlPresentation.ts): the bundled art for a
/// source-control definition `icon` key. `github` is not here: it is drawn from
/// the shell's lucide `Github` glyph, as the settings rows already do. A key
/// without art draws the change request glyph upstream.
pub fn source_control_icon_path(icon: &str) -> Option<&'static str> {
    match icon {
        "gitlab" => Some(GITLAB),
        "forgejo" => Some(FORGEJO),
        "bitbucket" => Some(BITBUCKET),
        "azure-devops" => Some(AZURE_DEVOPS),
        "gitcafe" => Some(GITCAFE),
        _ => None,
    }
}

/// The pull request list's empty-state drawing: a branch stopped short of the base line
/// (PullRequestListEmptyState.tsx `BranchMark`, unjoined).
pub const BRANCH_MARK: &str = "brand/branch-mark.svg";

/// lucide `smile-plus` (the reaction picker trigger); the gpui-kit icon set ships no `smile` family.
pub const SMILE_PLUS: &str = "brand/smile-plus.svg";

/// `@pierre/diffs` sprite icons (`dist/sprite.js`), one monochrome SVG each.
/// JetBrains product marks (`components/JetBrainsIcons.tsx`).
pub mod editor_icons {
    pub const ANTIGRAVITY: &str = "editors/antigravity.svg";
    pub const AQUA: &str = "editors/aqua.svg";
    pub const CLION: &str = "editors/clion.svg";
    pub const DATAGRIP: &str = "editors/datagrip.svg";
    pub const DATASPELL: &str = "editors/dataspell.svg";
    pub const FILE_EXPLORER: &str = "editors/file-explorer.svg";
    pub const FINDER: &str = "editors/finder.svg";
    pub const FOLDER_CLOSED: &str = "editors/folder-closed.svg";
    pub const GOLAND: &str = "editors/goland.svg";
    pub const IDEA: &str = "editors/idea.svg";
    pub const KIRO: &str = "editors/kiro.svg";
    pub const PHPSTORM: &str = "editors/phpstorm.svg";
    pub const PYCHARM: &str = "editors/pycharm.svg";
    pub const RIDER: &str = "editors/rider.svg";
    pub const RUBYMINE: &str = "editors/rubymine.svg";
    pub const RUSTROVER: &str = "editors/rustrover.svg";
    pub const TRAE: &str = "editors/trae.svg";
    pub const VSCODIUM: &str = "editors/vscodium.svg";
    pub const VSCODE: &str = "editors/vscode.svg";
    pub const VSCODE_INSIDERS: &str = "editors/vscode-insiders.svg";
    pub const WEBSTORM: &str = "editors/webstorm.svg";
    pub const ZED: &str = "editors/zed.svg";
}

pub mod diff_icons {
    pub const SYMBOL_ADDED: &str = "diff/symbol-added.svg";
    pub const SYMBOL_DELETED: &str = "diff/symbol-deleted.svg";
    pub const SYMBOL_MODIFIED: &str = "diff/symbol-modified.svg";
    pub const SYMBOL_MOVED: &str = "diff/symbol-moved.svg";
    pub const ARROW_RIGHT_SHORT: &str = "diff/arrow-right-short.svg";
    pub const EXPAND: &str = "diff/expand.svg";
    pub const EXPAND_UP: &str = "diff/expand-up.svg";
    pub const EXPAND_ALL: &str = "diff/expand-all.svg";
    pub const PLUS: &str = "diff/plus.svg";
}

const BRAND: &[(&str, &[u8])] = &[
    (
        "brand/openai.svg",
        include_bytes!("../assets/brand/openai.svg"),
    ),
    (
        "brand/claude.svg",
        include_bytes!("../assets/brand/claude.svg"),
    ),
    (
        "brand/cursor.svg",
        include_bytes!("../assets/brand/cursor.svg"),
    ),
    (
        "brand/opencode.svg",
        include_bytes!("../assets/brand/opencode.svg"),
    ),
    (SEND_ARROW, include_bytes!("../assets/brand/send-arrow.svg")),
    (
        CHEVRON_DOWN,
        include_bytes!("../assets/brand/chevron-down.svg"),
    ),
    (MAC_MINI, include_bytes!("../assets/brand/mac-mini.svg")),
    (MAC_STUDIO, include_bytes!("../assets/brand/mac-studio.svg")),
    (
        WORDMARK,
        include_bytes!("../../../assets/prod/wordmark.svg"),
    ),
    (
        PROVIDER_OPENAI,
        include_bytes!("../assets/brand/openai.svg"),
    ),
    (
        PROVIDER_CLAUDE,
        include_bytes!("../assets/brand/claude.svg"),
    ),
    (
        PROVIDER_CURSOR,
        include_bytes!("../assets/brand/cursor.svg"),
    ),
    (
        BRANCH_MARK,
        include_bytes!("../assets/brand/branch-mark.svg"),
    ),
    (SMILE_PLUS, include_bytes!("../assets/brand/smile-plus.svg")),
    (ULTRAFAST, include_bytes!("../assets/brand/ultrafast.svg")),
    (LINUX, include_bytes!("../assets/brand/linux.svg")),
    (GIT, include_bytes!("../assets/brand/git.svg")),
    (JUJUTSU, include_bytes!("../assets/brand/jujutsu.svg")),
    (GITLAB, include_bytes!("../assets/brand/gitlab.svg")),
    (
        GITLAB_MONOCHROME,
        include_bytes!("../assets/brand/gitlab-mono.svg"),
    ),
    (
        AZURE_DEVOPS,
        include_bytes!("../assets/brand/azure-devops.svg"),
    ),
    (BITBUCKET, include_bytes!("../assets/brand/bitbucket.svg")),
    (FORGEJO, include_bytes!("../assets/brand/forgejo.svg")),
    (GITCAFE, include_bytes!("../assets/brand/gitcafe.svg")),
    (
        ACP_REGISTRY,
        include_bytes!("../assets/brand/acp-registry.svg"),
    ),
    (
        MAC_SCREEN_RECORDING,
        include_bytes!("../assets/brand/mac-screen-recording.svg"),
    ),
    (
        MAC_ACCESSIBILITY,
        include_bytes!("../assets/brand/mac-accessibility.svg"),
    ),
    (
        editor_icons::ANTIGRAVITY,
        include_bytes!("../assets/editors/antigravity.svg"),
    ),
    (
        editor_icons::AQUA,
        include_bytes!("../assets/editors/aqua.svg"),
    ),
    (
        editor_icons::CLION,
        include_bytes!("../assets/editors/clion.svg"),
    ),
    (
        editor_icons::DATAGRIP,
        include_bytes!("../assets/editors/datagrip.svg"),
    ),
    (
        editor_icons::DATASPELL,
        include_bytes!("../assets/editors/dataspell.svg"),
    ),
    (
        editor_icons::FILE_EXPLORER,
        include_bytes!("../assets/editors/file-explorer.svg"),
    ),
    (
        editor_icons::FINDER,
        include_bytes!("../assets/editors/finder.svg"),
    ),
    (
        editor_icons::FOLDER_CLOSED,
        include_bytes!("../assets/editors/folder-closed.svg"),
    ),
    (
        editor_icons::GOLAND,
        include_bytes!("../assets/editors/goland.svg"),
    ),
    (
        editor_icons::IDEA,
        include_bytes!("../assets/editors/idea.svg"),
    ),
    (
        editor_icons::KIRO,
        include_bytes!("../assets/editors/kiro.svg"),
    ),
    (
        editor_icons::PHPSTORM,
        include_bytes!("../assets/editors/phpstorm.svg"),
    ),
    (
        editor_icons::PYCHARM,
        include_bytes!("../assets/editors/pycharm.svg"),
    ),
    (
        editor_icons::RIDER,
        include_bytes!("../assets/editors/rider.svg"),
    ),
    (
        editor_icons::RUBYMINE,
        include_bytes!("../assets/editors/rubymine.svg"),
    ),
    (
        editor_icons::RUSTROVER,
        include_bytes!("../assets/editors/rustrover.svg"),
    ),
    (
        editor_icons::TRAE,
        include_bytes!("../assets/editors/trae.svg"),
    ),
    (
        editor_icons::VSCODIUM,
        include_bytes!("../assets/editors/vscodium.svg"),
    ),
    (
        editor_icons::VSCODE,
        include_bytes!("../assets/editors/vscode.svg"),
    ),
    (
        editor_icons::VSCODE_INSIDERS,
        include_bytes!("../assets/editors/vscode-insiders.svg"),
    ),
    (
        editor_icons::WEBSTORM,
        include_bytes!("../assets/editors/webstorm.svg"),
    ),
    (
        editor_icons::ZED,
        include_bytes!("../assets/editors/zed.svg"),
    ),
    (
        diff_icons::SYMBOL_ADDED,
        include_bytes!("../assets/diff/symbol-added.svg"),
    ),
    (
        diff_icons::SYMBOL_DELETED,
        include_bytes!("../assets/diff/symbol-deleted.svg"),
    ),
    (
        diff_icons::SYMBOL_MODIFIED,
        include_bytes!("../assets/diff/symbol-modified.svg"),
    ),
    (
        diff_icons::SYMBOL_MOVED,
        include_bytes!("../assets/diff/symbol-moved.svg"),
    ),
    (
        diff_icons::ARROW_RIGHT_SHORT,
        include_bytes!("../assets/diff/arrow-right-short.svg"),
    ),
    (
        diff_icons::EXPAND,
        include_bytes!("../assets/diff/expand.svg"),
    ),
    (
        diff_icons::EXPAND_UP,
        include_bytes!("../assets/diff/expand-up.svg"),
    ),
    (
        diff_icons::EXPAND_ALL,
        include_bytes!("../assets/diff/expand-all.svg"),
    ),
    (diff_icons::PLUS, include_bytes!("../assets/diff/plus.svg")),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = crate::ui::pierre_icons::load_pierre_icon(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = AppIcons.load(path)? {
            return Ok(Some(bytes));
        }
        if let Some(bytes) = UsageIcons.load(path)? {
            return Ok(Some(bytes));
        }
        if let Some(bytes) = crate::settings::icons::load_brand(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = crate::settings::icons::SettingsIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names: Vec<SharedString> = BRAND
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| name.starts_with(path))
            .map(SharedString::from)
            .collect();
        names.extend(
            crate::ui::pierre_icons::pierre_icon_asset_paths()
                .into_iter()
                .filter(|name| name.starts_with(path))
                .map(SharedString::from),
        );
        names.extend(AppIcons.list(path)?);
        names.extend(UsageIcons.list(path)?);
        names.extend(crate::settings::icons::SettingsIcons.list(path)?);
        names.extend(Assets.list(path)?);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_icons_marks_load_by_path() {
        for path in [
            ULTRAFAST,
            LINUX,
            GIT,
            JUJUTSU,
            GITLAB,
            GITLAB_MONOCHROME,
            AZURE_DEVOPS,
            BITBUCKET,
            FORGEJO,
            GITCAFE,
            ACP_REGISTRY,
            MAC_SCREEN_RECORDING,
            MAC_ACCESSIBILITY,
        ] {
            let bytes = AppAssets
                .load(path)
                .unwrap()
                .unwrap_or_else(|| panic!("{path} is not embedded"));
            assert!(bytes.starts_with(b"<svg"), "{path}");
        }
    }

    #[test]
    fn source_control_icon_keys_map_to_bundled_art() {
        for (key, path) in [
            ("gitlab", GITLAB),
            ("forgejo", FORGEJO),
            ("bitbucket", BITBUCKET),
            ("azure-devops", AZURE_DEVOPS),
            ("gitcafe", GITCAFE),
        ] {
            assert_eq!(source_control_icon_path(key), Some(path), "{key}");
        }
        assert_eq!(source_control_icon_path("github"), None);
        assert_eq!(source_control_icon_path("unknown"), None);
    }

    #[test]
    fn gitlab_monochrome_has_no_fixed_colours() {
        let bytes = AppAssets.load(GITLAB_MONOCHROME).unwrap().unwrap();
        let svg = std::str::from_utf8(&bytes).unwrap();
        assert!(!svg.contains("#E24329") && svg.matches("currentColor").count() == 4);
    }
}

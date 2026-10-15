//! Asset source: the lucide icons the UI uses (upstream imports them from
//! `lucide-react`) and the composer's send glyph, falling back to gpui-kit's default component icons.
//!
//! Only the icons listed in [`AppIcons`] are embedded; add one here before
//! using it, otherwise it renders blank.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

icon_assets!(
    pub AppIcons,
    [
        AlarmClock,
        AlarmClockOff,
        Archive,
        ArrowLeft,
        ArrowRight,
        ArrowRightLeft,
        ArrowUp,
        Bot,
        Brain,
        ChartNoAxesColumn,
        Check,
        ChevronDown,
        ChevronRight,
        ChevronsDownUp,
        ChevronsUpDown,
        CircleAlert,
        CircleCheck,
        CircleDashed,
        CircleX,
        Code,
        Copy,
        Ellipsis,
        ChevronUp,
        Clock,
        Eye,
        FileDiff,
        FileText,
        Folder,
        FolderOpen,
        FolderCode,
        FolderGit2,
        FolderPlus,
        FolderTree,
        GitBranch,
        GitFork,
        Github,
        GitPullRequestArrow,
        Globe,
        Hammer,
        Info,
        Lightbulb,
        Link,
        GitMerge,
        GitPullRequestClosed,
        GitPullRequestDraft,
        Hash,
        Link2,
        ListChecks,
        ListFilter,
        LoaderCircle,
        Maximize2,
        MessageCircle,
        MessageSquare,
        MessageSquareWarning,
        MailOpen,
        MessageCircleQuestionMark,
        MessageSquarePlus,
        Minimize2,
        Minus,
        OctagonAlert,
        PanelLeft,
        PanelLeftClose,
        PanelRight,
        Paperclip,
        Pencil,
        Pin,
        PinOff,
        Plus,
        Redo2,
        Search,
        Settings,
        Shield,
        Smartphone,
        Square,
        SquareCheck,
        SquareMenu,
        SquarePen,
        Terminal,
        TextWrap,
        TriangleAlert,
        Undo2,
        Wrench,
        RefreshCw,
        ShieldQuestionMark,
        Timer,
        Trash,
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
        GitCommitHorizontal,
        Laptop,
        Lock,
        MessageSquareDashed,
        Monitor,
        Moon,
        Palette,
        RotateCcw,
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

const BRAND: &[(&str, &[u8])] = &[
    (SEND_ARROW, include_bytes!("../assets/brand/send-arrow.svg")),
    (
        CHEVRON_DOWN,
        include_bytes!("../assets/brand/chevron-down.svg"),
    ),
    (MAC_MINI, include_bytes!("../assets/brand/mac-mini.svg")),
    (MAC_STUDIO, include_bytes!("../assets/brand/mac-studio.svg")),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = AppIcons.load(path)? {
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
        names.extend(AppIcons.list(path)?);
        names.extend(Assets.list(path)?);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

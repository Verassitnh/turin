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
        Archive,
        ArrowLeft,
        ArrowRight,
        ArrowUp,
        Blocks,
        Bookmark,
        Bot,
        Brain,
        ChartNoAxesColumn,
        Check,
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
        Code,
        Copy,
        CornerUpRight,
        Ellipsis,
        Eye,
        File,
        FileDiff,
        FileText,
        Folder,
        FolderOpen,
        FolderPlus,
        GitBranch,
        GitFork,
        Github,
        GitPullRequestArrow,
        Globe,
        Hammer,
        Image,
        Info,
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
        MessageSquareWarning,
        MessagesSquare,
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
        Pin,
        Play,
        Plus,
        Redo2,
        RefreshCw,
        Search,
        Settings,
        Shield,
        Smartphone,
        Sparkles,
        Square,
        SquareCheck,
        SquareMenu,
        SquarePen,
        Star,
        Terminal,
        TextWrap,
        TriangleAlert,
        Undo2,
        UserRound,
        Wrench,
        X,
        Zap,
    ]
);

/// The composer's send arrow (ComposerPrimaryActions.tsx).
pub const SEND_ARROW: &str = "brand/send-arrow.svg";

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

const BRAND: &[(&str, &[u8])] = &[
    (SEND_ARROW, include_bytes!("../assets/brand/send-arrow.svg")),
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

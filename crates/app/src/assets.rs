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
        ArrowDownUp,
        ArrowLeft,
        ArrowRight,
        ArrowUp,
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
        Circle,
        CircleAlert,
        CircleCheck,
        CircleDashed,
        CircleDot,
        CircleX,
        Clock,
        Code,
        Columns2,
        Copy,
        CornerDownRight,
        Ellipsis,
        ExternalLink,
        Eye,
        FileCode,
        FileDiff,
        Files,
        FileText,
        Flag,
        Folder,
        FolderOpen,
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
        Globe,
        Hammer,
        Hourglass,
        Info,
        Layers,
        Lightbulb,
        Link,
        Link2,
        ListChecks,
        ListFilter,
        LoaderCircle,
        Maximize2,
        MessageCircle,
        MessageSquare,
        MessageSquareOff,
        MessageSquareText,
        MessageSquareWarning,
        Minimize2,
        Minus,
        OctagonAlert,
        PanelLeft,
        PanelLeftClose,
        PanelRight,
        Paperclip,
        Pencil,
        PenLine,
        Pilcrow,
        Pin,
        Play,
        Plug2,
        Plus,
        Redo2,
        RefreshCw,
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
        Tag,
        Terminal,
        TextWrap,
        ThumbsUp,
        Trash,
        TriangleAlert,
        Undo2,
        Unlink2,
        UserCheck,
        UserLock,
        UserPlus,
        UserRound,
        UserRoundX,
        Users,
        Wrench,
        X,
        Zap,
    ]
);

/// The composer's send arrow (ComposerPrimaryActions.tsx).
pub const SEND_ARROW: &str = "brand/send-arrow.svg";

/// The pull request list's empty-state drawing: a branch stopped short of the base line
/// (PullRequestListEmptyState.tsx `BranchMark`, unjoined).
pub const BRANCH_MARK: &str = "brand/branch-mark.svg";

/// lucide `smile-plus` (the reaction picker trigger); the gpui-kit icon set ships no `smile` family.
pub const SMILE_PLUS: &str = "brand/smile-plus.svg";

const BRAND: &[(&str, &[u8])] = &[
    (SEND_ARROW, include_bytes!("../assets/brand/send-arrow.svg")),
    (BRANCH_MARK, include_bytes!("../assets/brand/branch-mark.svg")),
    (SMILE_PLUS, include_bytes!("../assets/brand/smile-plus.svg")),
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

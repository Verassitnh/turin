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
        Unlink2,
        UserCheck,
        UserLock,
        UserPlus,
        UserRoundX,
        Users,
        X,
        Zap,
    ]
);

/// The composer's send arrow (ComposerPrimaryActions.tsx).
pub const SEND_ARROW: &str = "brand/send-arrow.svg";
/// Lucide `chevron-down` at `stroke-width` 2.25 (ComposerControl.tsx); the
/// stock `IconName::ChevronDown` is stroke 2.
pub const CHEVRON_DOWN: &str = "brand/chevron-down.svg";

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

/// The pull request list's empty-state drawing: a branch stopped short of the base line
/// (PullRequestListEmptyState.tsx `BranchMark`, unjoined).
pub const BRANCH_MARK: &str = "brand/branch-mark.svg";

/// lucide `smile-plus` (the reaction picker trigger); the gpui-kit icon set ships no `smile` family.
pub const SMILE_PLUS: &str = "brand/smile-plus.svg";

/// `@pierre/diffs` sprite icons (`dist/sprite.js`), one monochrome SVG each.
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
    (SEND_ARROW, include_bytes!("../assets/brand/send-arrow.svg")),
    (
        CHEVRON_DOWN,
        include_bytes!("../assets/brand/chevron-down.svg"),
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
        if let Some(bytes) = AppIcons.load(path)? {
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
        names.extend(AppIcons.list(path)?);
        names.extend(crate::settings::icons::SettingsIcons.list(path)?);
        names.extend(Assets.list(path)?);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

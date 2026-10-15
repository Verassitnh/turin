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
        Columns2,
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
        FolderTree,
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
        Pilcrow,
        Pin,
        PinOff,
        Plus,
        Redo2,
        RefreshCw,
        RotateCw,
        Rows3,
        Search,
        Settings,
        Shield,
        Smartphone,
        Square,
        SquareCheck,
        SquareMenu,
        SquarePen,
        Terminal,
        Trash,
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
    ]
);

/// The composer's send arrow (ComposerPrimaryActions.tsx).
pub const SEND_ARROW: &str = "brand/send-arrow.svg";
/// Lucide `chevron-down` at `stroke-width` 2.25 (ComposerControl.tsx); the
/// stock `IconName::ChevronDown` is stroke 2.
pub const CHEVRON_DOWN: &str = "brand/chevron-down.svg";
/// The bare T mark for the sidebar brand slot (upstream `T3Wordmark`). Turin's
/// `prod/wordmark.svg`, `currentColor`.
pub const WORDMARK: &str = "brand/wordmark.svg";
/// The macOS app icon (`prod/black-macos-1024.png`), set as the Dock icon of an
/// unbundled run. Bundles get their icon from `Turin.icns`.
pub const APP_ICON_PNG: &[u8] = include_bytes!("../assets/brand/app-icon.png");

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
    (WORDMARK, include_bytes!("../assets/brand/wordmark.svg")),
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

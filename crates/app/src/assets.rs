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
        ArrowRightLeft,
        ArrowUp,
        Bot,
        ChartNoAxesColumn,
        Check,
        ChevronDown,
        ChevronRight,
        ChevronUp,
        CircleAlert,
        CircleCheck,
        CircleDashed,
        Clock,
        Copy,
        Eye,
        FileDiff,
        Folder,
        FolderCode,
        FolderGit2,
        FolderPlus,
        FolderTree,
        GitBranch,
        GitMerge,
        GitPullRequestArrow,
        GitPullRequestClosed,
        GitPullRequestDraft,
        Hash,
        Link2,
        ListFilter,
        MailOpen,
        Maximize2,
        MessageCircleQuestionMark,
        MessageSquarePlus,
        Minimize2,
        PanelLeft,
        PanelLeftClose,
        PanelRight,
        Paperclip,
        Pencil,
        Pin,
        PinOff,
        Plus,
        RefreshCw,
        Search,
        Settings,
        ShieldQuestionMark,
        SquareMenu,
        SquarePen,
        Terminal,
        Timer,
        Trash,
        Undo2,
        X,
    ]
);

/// The composer's send arrow (ComposerPrimaryActions.tsx).
pub const SEND_ARROW: &str = "brand/send-arrow.svg";

const BRAND: &[(&str, &[u8])] = &[(SEND_ARROW, include_bytes!("../assets/brand/send-arrow.svg"))];

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

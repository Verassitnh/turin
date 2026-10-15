//! Asset source: the lucide icons the UI uses (upstream imports them from
//! `lucide-react`), the T3 wordmark (port of
//! upstream/apps/web/src/components/T3Wordmark.tsx) and the composer's send
//! glyph, falling back to gpui-kit's default component icons.
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
        ArrowLeft,
        ArrowUp,
        ChartNoAxesColumn,
        Check,
        ChevronDown,
        ChevronRight,
        CircleAlert,
        CircleCheck,
        CircleDashed,
        FileDiff,
        Folder,
        FolderPlus,
        GitBranch,
        GitPullRequest,
        ListFilter,
        Maximize2,
        Minimize2,
        PanelLeft,
        PanelLeftClose,
        PanelRight,
        Paperclip,
        Pin,
        Plus,
        Search,
        Settings,
        SquareMenu,
        SquarePen,
        X,
    ]
);

/// The T3 wordmark ("T3"), filled with the current text color.
pub const T3_WORDMARK: &str = "brand/t3-wordmark.svg";
/// The composer's send arrow (ComposerPrimaryActions.tsx).
pub const SEND_ARROW: &str = "brand/send-arrow.svg";

const BRAND: &[(&str, &[u8])] = &[
    (
        T3_WORDMARK,
        include_bytes!("../assets/brand/t3-wordmark.svg"),
    ),
    (SEND_ARROW, include_bytes!("../assets/brand/send-arrow.svg")),
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

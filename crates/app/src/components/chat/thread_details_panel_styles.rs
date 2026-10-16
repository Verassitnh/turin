//! Port of upstream/apps/web/src/components/chat/threadDetailsPanelStyles.ts

use gpui_kit::{Pixels, px, rems};

/// `size-4` icons in rows.
pub const THREAD_DETAILS_ICON_SIZE: gpui_kit::Rems = rems(1.);
/// `text-2xs` meta labels.
pub const THREAD_DETAILS_META_SIZE: gpui_kit::Rems = rems(0.6875);
/// `ps-6.5` nested workflow list.
pub const THREAD_DETAILS_WORKFLOW_INDENT: Pixels = px(26.);
/// Bounded lineage list `max-h-[13.5rem]`.
pub const THREAD_DETAILS_LINEAGE_MAX_HEIGHT: gpui_kit::Rems = rems(13.5);

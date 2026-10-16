//! Port of upstream/apps/web/src/components/ui: the primitives every screen
//! is assembled from, keeping upstream's component and variant names. Each
//! module documents what it maps onto in gpui-component and what is not
//! ported.

pub mod badge;
pub mod button;
pub mod collapsible_section_header;
pub mod color;
pub mod command;
pub mod dialog;
pub mod empty;
pub mod icon;
pub mod kbd;
pub mod menu;
pub mod middle_truncate;
pub mod pierre_icons;
pub mod popover;
pub mod scroll_area;
pub mod separator;
pub mod sidebar;
pub mod skeleton;
pub mod text_input;
pub mod toggle;
pub mod tooltip;

#[allow(unused_imports)]
pub use badge::{Badge, BadgeSize, BadgeVariant};
#[allow(unused_imports)]
pub use button::{Button, ButtonSize, ButtonVariant};
#[allow(unused_imports)]
pub use icon::{Icon, IconName};
#[allow(unused_imports)]
pub use kbd::{Kbd, KbdGroup};
#[allow(unused_imports)]
pub use popover::{
    PopoverOptions, PopoverPadding, PopoverVariant, PopoverWidth, popover, popover_title,
};
#[allow(unused_imports)]
pub use scroll_area::{scroll_area, scroll_area_hidden};
#[allow(unused_imports)]
pub use separator::Separator;
#[allow(unused_imports)]
pub use toggle::{Toggle, ToggleSize, ToggleVariant};
#[allow(unused_imports)]
pub use tooltip::{Tooltip, TooltipVariant};

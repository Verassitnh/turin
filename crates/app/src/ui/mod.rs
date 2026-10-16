//! Port of upstream/apps/web/src/components/ui: the primitives every screen
//! is assembled from, keeping upstream's component and variant names. Each
//! module documents what it maps onto in gpui-component and what is not
//! ported.

pub mod animated_size;
pub mod badge;
pub mod button;
pub mod collapsible_section_header;
pub mod dialog;
pub mod discovery_list;
pub mod draft_input;
pub mod empty;
pub mod group;
pub mod icon;
pub mod input_group;
pub mod kbd;
pub mod label;
pub mod menu;
pub mod number_field;
pub mod popover;
pub mod radio_group;
pub mod scroll_area;
pub mod separator;
pub mod sidebar;
pub mod skeleton;
pub mod standalone_page;
pub mod start_truncated_path;
pub mod switch;
pub mod t3_wordmark;
pub mod table;
pub mod text_input;
pub mod textarea;
pub mod toggle;
pub mod tooltip;
pub mod workspace_page;

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

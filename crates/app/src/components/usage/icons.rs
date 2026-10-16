//! Lucide icons the Usage page and the welcome wizard use on top of the
//! shell's set ([`crate::assets::AppIcons`]); chained into
//! [`crate::assets::AppAssets`].

use gpui_kit::assets::icon_assets;

icon_assets!(
    pub UsageIcons,
    [
        Database,
        Gauge,
        Monitor,
        RotateCcw,
        SlidersHorizontal,
        Ticket,
        TrendingDown,
        TrendingUp,
    ]
);

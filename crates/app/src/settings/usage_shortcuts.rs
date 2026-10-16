//! The slice of upstream/apps/web/src/components/usage/usageShortcuts.ts the
//! keybindings page needs (command labels and order). The Usage page itself
//! is N4's.

pub struct MetricOption {
    pub value: &'static str,
    pub label: &'static str,
    pub command: &'static str,
}

pub struct WindowOption {
    pub days: u32,
    pub label: &'static str,
    pub command: &'static str,
}

pub const METRIC_OPTIONS: [MetricOption; 3] = [
    MetricOption {
        value: "cost",
        label: "Cost",
        command: "usage.cost",
    },
    MetricOption {
        value: "tokens",
        label: "Tokens",
        command: "usage.tokens",
    },
    MetricOption {
        value: "limits",
        label: "Limits",
        command: "usage.limits",
    },
];

pub const WINDOW_OPTIONS: [WindowOption; 4] = [
    WindowOption {
        days: 1,
        label: "Past 24h",
        command: "usage.period.day",
    },
    WindowOption {
        days: 7,
        label: "7 days",
        command: "usage.period.week",
    },
    WindowOption {
        days: 30,
        label: "30 days",
        command: "usage.period.month",
    },
    WindowOption {
        days: 90,
        label: "90 days",
        command: "usage.period.quarter",
    },
];

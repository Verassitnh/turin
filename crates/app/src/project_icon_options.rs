//! Port of upstream/apps/web/src/projectIconOptions.ts (+ test).

#![allow(dead_code)]

#[allow(unused_imports)]
pub use crate::project_icon_colors::{PROJECT_ICON_COLORS, project_icon_color};

use crate::state::models::ProjectIconColor;
use crate::ui::IconName;
use regex::Regex;
use std::sync::LazyLock;
use unicode_segmentation::UnicodeSegmentation;

const POPULAR_PROJECT_ICONS: &[IconName] = &[
    IconName::FolderCode,
    IconName::Code,
    IconName::Terminal,
    IconName::Globe,
    IconName::Server,
    IconName::Database,
    IconName::Bot,
    IconName::Sparkles,
    IconName::Smartphone,
    IconName::Monitor,
    IconName::CloudCog,
    IconName::Package,
    IconName::BookOpen,
    IconName::FlaskConical,
    IconName::ShieldCheck,
    IconName::Rocket,
    IconName::Gamepad2,
    IconName::Music,
    IconName::Image,
    IconName::ShoppingBag,
    IconName::GitBranch,
    IconName::Workflow,
    IconName::Wrench,
    IconName::Layers,
];

/// Every embedded app icon, searchable by kebab-case name (Lucide paths).
const SEARCHABLE_ICONS: &[IconName] = &[
    IconName::Server,
    IconName::Database,
    IconName::Monitor,
    IconName::CloudCog,
    IconName::FlaskConical,
    IconName::Gamepad2,
    IconName::Music,
    IconName::ShoppingBag,
    IconName::Workflow,
    IconName::AlarmClock,
    IconName::AlarmClockOff,
    IconName::Archive,
    IconName::ArrowDownUp,
    IconName::ArrowLeft,
    IconName::ArrowRight,
    IconName::ArrowRightLeft,
    IconName::ArrowUp,
    IconName::Blocks,
    IconName::Bookmark,
    IconName::ArrowUpRight,
    IconName::BookOpen,
    IconName::Bot,
    IconName::Brain,
    IconName::CalendarArrowDown,
    IconName::CalendarArrowUp,
    IconName::ChartNoAxesColumn,
    IconName::Check,
    IconName::CheckCheck,
    IconName::ChevronDown,
    IconName::ChevronLeft,
    IconName::ChevronRight,
    IconName::ChevronsDownUp,
    IconName::ChevronsUpDown,
    IconName::ChevronUp,
    IconName::CircleAlert,
    IconName::CircleCheck,
    IconName::CircleDashed,
    IconName::CircleGauge,
    IconName::CircleX,
    IconName::Clock3,
    IconName::Circle,
    IconName::CircleDot,
    IconName::Clock,
    IconName::Code,
    IconName::Columns2,
    IconName::Copy,
    IconName::CornerUpRight,
    IconName::Ellipsis,
    IconName::Eye,
    IconName::File,
    IconName::CornerDownRight,
    IconName::ExternalLink,
    IconName::FileCode,
    IconName::FileDiff,
    IconName::Files,
    IconName::FileText,
    IconName::Flag,
    IconName::Folder,
    IconName::FolderOpen,
    IconName::FolderCode,
    IconName::FolderGit2,
    IconName::FolderPlus,
    IconName::FolderTree,
    IconName::GitBranch,
    IconName::GitCommitHorizontal,
    IconName::GitFork,
    IconName::Github,
    IconName::GitMerge,
    IconName::GitPullRequestArrow,
    IconName::GitPullRequestClosed,
    IconName::GitPullRequestDraft,
    IconName::Hash,
    IconName::Globe,
    IconName::Hammer,
    IconName::Image,
    IconName::Hourglass,
    IconName::Info,
    IconName::Layers,
    IconName::Lightbulb,
    IconName::GitPullRequest,
    IconName::GripVertical,
    IconName::Link,
    IconName::Link2,
    IconName::ListChecks,
    IconName::ListFilter,
    IconName::ListOrdered,
    IconName::ListPlus,
    IconName::Lock,
    IconName::LockOpen,
    IconName::LoaderCircle,
    IconName::Maximize2,
    IconName::MessageCircle,
    IconName::MessageSquare,
    IconName::MessageSquareOff,
    IconName::MessageSquareText,
    IconName::MessageSquareWarning,
    IconName::MessagesSquare,
    IconName::MailOpen,
    IconName::MessageCircleQuestionMark,
    IconName::MessageSquarePlus,
    IconName::Minimize2,
    IconName::Minus,
    IconName::OctagonAlert,
    IconName::Package,
    IconName::PanelLeft,
    IconName::PanelLeftClose,
    IconName::PanelRight,
    IconName::Paperclip,
    IconName::PenLine,
    IconName::Pencil,
    IconName::PencilRuler,
    IconName::Pilcrow,
    IconName::Pin,
    IconName::Play,
    IconName::PinOff,
    IconName::Plug2,
    IconName::Plus,
    IconName::Redo2,
    IconName::RefreshCw,
    IconName::ShieldQuestionMark,
    IconName::Timer,
    IconName::RotateCw,
    IconName::Reply,
    IconName::Rocket,
    IconName::RotateCcw,
    IconName::Route,
    IconName::Rows3,
    IconName::Search,
    IconName::Send,
    IconName::Settings,
    IconName::Settings2,
    IconName::Shield,
    IconName::ShieldCheck,
    IconName::Smartphone,
    IconName::Sparkles,
    IconName::Square,
    IconName::SquareCheck,
    IconName::SquareMenu,
    IconName::SquarePen,
    IconName::Star,
    IconName::Tag,
    IconName::Terminal,
    IconName::TextWrap,
    IconName::ThumbsUp,
    IconName::Trash,
    IconName::TriangleAlert,
    IconName::Undo2,
    IconName::UserRound,
    IconName::Wrench,
    IconName::Unlink2,
    IconName::UserCheck,
    IconName::UserLock,
    IconName::UserPlus,
    IconName::UserRoundX,
    IconName::Users,
    IconName::X,
    IconName::Zap,
];

pub struct ProjectEmoji {
    pub emoji: &'static str,
    pub label: &'static str,
}

pub const PROJECT_EMOJIS: &[ProjectEmoji] = &[
    ProjectEmoji {
        emoji: "💻",
        label: "Computer",
    },
    ProjectEmoji {
        emoji: "🛠️",
        label: "Tools",
    },
    ProjectEmoji {
        emoji: "🚀",
        label: "Rocket",
    },
    ProjectEmoji {
        emoji: "🤖",
        label: "Robot",
    },
    ProjectEmoji {
        emoji: "✨",
        label: "Sparkles",
    },
    ProjectEmoji {
        emoji: "⚡",
        label: "Lightning",
    },
    ProjectEmoji {
        emoji: "🌐",
        label: "Web",
    },
    ProjectEmoji {
        emoji: "📱",
        label: "Mobile",
    },
    ProjectEmoji {
        emoji: "🖥️",
        label: "Desktop",
    },
    ProjectEmoji {
        emoji: "⌨️",
        label: "Keyboard",
    },
    ProjectEmoji {
        emoji: "⚙️",
        label: "Gear",
    },
    ProjectEmoji {
        emoji: "🗄️",
        label: "Database",
    },
    ProjectEmoji {
        emoji: "☁️",
        label: "Cloud",
    },
    ProjectEmoji {
        emoji: "📦",
        label: "Package",
    },
    ProjectEmoji {
        emoji: "📚",
        label: "Books",
    },
    ProjectEmoji {
        emoji: "🧪",
        label: "Test tube",
    },
    ProjectEmoji {
        emoji: "🔒",
        label: "Lock",
    },
    ProjectEmoji {
        emoji: "🎮",
        label: "Game",
    },
    ProjectEmoji {
        emoji: "🎵",
        label: "Music",
    },
    ProjectEmoji {
        emoji: "🎬",
        label: "Movie",
    },
    ProjectEmoji {
        emoji: "🖼️",
        label: "Picture",
    },
    ProjectEmoji {
        emoji: "🛍️",
        label: "Shopping",
    },
    ProjectEmoji {
        emoji: "🔥",
        label: "Fire",
    },
    ProjectEmoji {
        emoji: "💡",
        label: "Idea",
    },
    ProjectEmoji {
        emoji: "🧩",
        label: "Puzzle",
    },
    ProjectEmoji {
        emoji: "📊",
        label: "Chart",
    },
    ProjectEmoji {
        emoji: "🧠",
        label: "Brain",
    },
    ProjectEmoji {
        emoji: "🦄",
        label: "Unicorn",
    },
    ProjectEmoji {
        emoji: "🐙",
        label: "Octopus",
    },
    ProjectEmoji {
        emoji: "🌱",
        label: "Seedling",
    },
];

fn icon_kebab_name(name: IconName) -> String {
    let path = name.path().to_string();
    path.rsplit('/')
        .next()
        .unwrap_or(path.as_str())
        .trim_end_matches(".svg")
        .to_owned()
}

pub fn filter_project_icon_names(query: &str) -> Vec<IconName> {
    let normalized = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if normalized.is_empty() {
        return POPULAR_PROJECT_ICONS.to_vec();
    }
    SEARCHABLE_ICONS
        .iter()
        .copied()
        .filter(|name| icon_kebab_name(*name).contains(&normalized))
        .take(60)
        .collect()
}

/// `/^\p{Regional_Indicator}{2}$/u`.
static FLAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Regional_Indicator}{2}$").expect("flag pattern"));
/// `/^[#*0-9]\uFE0F?\u20E3$/u`: a keycap needs its combining enclosing keycap.
static KEYCAP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[#*0-9]\x{FE0F}?\x{20E3}$").expect("keycap pattern"));
/// `/\p{Extended_Pictographic}/u`.
static EXTENDED_PICTOGRAPHIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\p{Extended_Pictographic}").expect("extended pictographic pattern")
});

pub fn first_emoji(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let segment = trimmed.graphemes(true).next()?;
    if !EXTENDED_PICTOGRAPHIC.is_match(segment)
        && !FLAG.is_match(segment)
        && !KEYCAP.is_match(segment)
    {
        return None;
    }
    Some(segment.to_owned())
}

/// Tailwind class string persisted in settings (GPUI maps via [`project_icon_color`]).
pub fn project_icon_color_class_name(color: ProjectIconColor) -> String {
    let slug = match color {
        ProjectIconColor::Gray => "gray",
        ProjectIconColor::Red => "red",
        ProjectIconColor::Orange => "orange",
        ProjectIconColor::Amber => "amber",
        ProjectIconColor::Yellow => "yellow",
        ProjectIconColor::Lime => "lime",
        ProjectIconColor::Green => "green",
        ProjectIconColor::Emerald => "emerald",
        ProjectIconColor::Teal => "teal",
        ProjectIconColor::Cyan => "cyan",
        ProjectIconColor::Sky => "sky",
        ProjectIconColor::Blue => "blue",
        ProjectIconColor::Indigo => "indigo",
        ProjectIconColor::Violet => "violet",
        ProjectIconColor::Purple => "purple",
        ProjectIconColor::Fuchsia => "fuchsia",
        ProjectIconColor::Pink => "pink",
        ProjectIconColor::Rose => "rose",
    };
    format!("text-{slug}-600 dark:text-{slug}-400")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_embedded_icons() {
        let names: Vec<String> = filter_project_icon_names("alarm clock")
            .into_iter()
            .map(icon_kebab_name)
            .collect();
        assert!(names.iter().any(|name| name == "alarm-clock"));
        let tab: Vec<String> = filter_project_icon_names("alarm  \tclock")
            .into_iter()
            .map(icon_kebab_name)
            .collect();
        assert!(tab.iter().any(|name| name == "alarm-clock"));
    }

    #[test]
    fn extracts_one_complete_emoji_grapheme() {
        assert_eq!(first_emoji("  👩🏽‍💻 hello").as_deref(), Some("👩🏽‍💻"));
        assert_eq!(first_emoji("🇺🇸 project").as_deref(), Some("🇺🇸"));
        assert_eq!(first_emoji("1️⃣ project").as_deref(), Some("1️⃣"));
        assert!(first_emoji("plain text").is_none());
    }

    #[test]
    fn a_keycap_needs_its_enclosing_keycap() {
        // Digits, `#` and `*` alone are text, however a name begins.
        for text in ["1 project", "#", "# tag", "*", "7", "0day"] {
            assert_eq!(first_emoji(text), None, "{text}");
        }
        assert_eq!(
            first_emoji("#\u{FE0F}\u{20E3} tag").as_deref(),
            Some("#\u{FE0F}\u{20E3}")
        );
        assert_eq!(first_emoji("*\u{20E3}").as_deref(), Some("*\u{20E3}"));
        assert_eq!(first_emoji("9\u{20E3} nine").as_deref(), Some("9\u{20E3}"));
        // The keycap is not the whole of what a segment may be: a digit with a stray mark is not it.
        assert_eq!(first_emoji("1\u{FE0F} project"), None);
    }

    #[test]
    fn every_extended_pictographic_counts_not_only_the_main_blocks() {
        // Outside U+1F300..1FAFF and U+2600..27BF, which the hand-written ranges were.
        for emoji in [
            "\u{2B50}",
            "\u{231A}",
            "\u{25B6}\u{FE0F}",
            "\u{1F170}\u{FE0F}",
            "\u{00A9}",
            "\u{2714}\u{FE0F}",
            "\u{1F680}",
        ] {
            assert_eq!(
                first_emoji(&format!("{emoji} app")).as_deref(),
                Some(emoji),
                "{emoji:?}"
            );
        }
        // Dingbats that are plain symbols, not emoji.
        assert_eq!(first_emoji("\u{2713} app"), None);
        assert_eq!(first_emoji("\u{2192} app"), None);
    }

    #[test]
    fn the_popular_icons_are_upstreams_twenty_four_in_order() {
        let names: Vec<String> = filter_project_icon_names("")
            .into_iter()
            .map(icon_kebab_name)
            .collect();
        assert_eq!(names.len(), 24);
        assert_eq!(&names[4..7], ["server", "database", "bot"]);
        assert_eq!(names[21], "workflow");
    }

    #[test]
    fn maps_persisted_colors_to_theme_aware_classes() {
        assert_eq!(
            project_icon_color_class_name(ProjectIconColor::Violet),
            "text-violet-600 dark:text-violet-400"
        );
    }
}

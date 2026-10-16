//! Port of upstream/apps/web/src/projectIconOptions.ts (+ test).

#![allow(dead_code)]

#[allow(unused_imports)]
pub use crate::project_icon_colors::{PROJECT_ICON_COLORS, project_icon_color};

use crate::state::models::ProjectIconColor;
use crate::ui::IconName;
use unicode_segmentation::UnicodeSegmentation;

const POPULAR_PROJECT_ICONS: &[IconName] = &[
    IconName::FolderCode,
    IconName::Code,
    IconName::Terminal,
    IconName::Globe,
    IconName::Package,
    IconName::BookOpen,
    IconName::Bot,
    IconName::Sparkles,
    IconName::Smartphone,
    IconName::ShieldCheck,
    IconName::Rocket,
    IconName::Image,
    IconName::GitBranch,
    IconName::Wrench,
    IconName::Layers,
];

/// Every embedded app icon, searchable by kebab-case name (Lucide paths).
const SEARCHABLE_ICONS: &[IconName] = &[
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

pub fn first_emoji(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let segment = trimmed.graphemes(true).next()?.to_owned();
    let is_flag = segment.chars().count() == 2
        && segment
            .chars()
            .all(|ch| ('\u{1F1E6}'..='\u{1F1FF}').contains(&ch));
    let is_keycap = segment.chars().count() <= 3
        && segment
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, '#' | '*' | '0'..='9'));
    let extended_pictographic = segment.chars().any(|ch| {
        matches!(
            ch,
            '\u{1F300}'..='\u{1FAFF}' | '\u{2600}'..='\u{27BF}'
        )
    });
    if !extended_pictographic && !is_flag && !is_keycap {
        return None;
    }
    Some(segment)
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
    fn maps_persisted_colors_to_theme_aware_classes() {
        assert_eq!(
            project_icon_color_class_name(ProjectIconColor::Violet),
            "text-violet-600 dark:text-violet-400"
        );
    }
}

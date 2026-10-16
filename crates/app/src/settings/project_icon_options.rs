//! Port of upstream/apps/web/src/projectIconOptions.ts and
//! upstream/apps/web/src/projectIconOptions.test.ts.
//!
//! The project icon picker's catalog: the popular Lucide icons, the full Lucide
//! name list searched by the picker, the emoji grid and `firstEmoji`.
//!
//! Deviation: `projectIconColorClassName` is `project_icon_color` in
//! `project_icon_colors.rs` (colors resolve to values, not CSS classes).

use unicode_segmentation::UnicodeSegmentation as _;

pub use crate::project_icon_colors::PROJECT_ICON_COLORS;

/// The icon shown first and for a project without a Lucide icon.
pub const DEFAULT_ICON: &str = "folder-code";

/// `POPULAR_PROJECT_ICONS`.
pub const POPULAR_PROJECT_ICONS: &[&str] = &[
    "folder-code",
    "code-2",
    "terminal",
    "globe-2",
    "server",
    "database",
    "bot",
    "sparkles",
    "smartphone",
    "monitor",
    "cloud-cog",
    "package",
    "book-open",
    "flask-conical",
    "shield-check",
    "rocket",
    "gamepad-2",
    "music",
    "image",
    "shopping-bag",
    "git-branch",
    "workflow",
    "wrench",
    "layers-3",
];

/// `PROJECT_EMOJIS`: emoji and label.
pub const PROJECT_EMOJIS: &[(&str, &str)] = &[
    ("💻", "Computer"),
    ("🛠️", "Tools"),
    ("🚀", "Rocket"),
    ("🤖", "Robot"),
    ("✨", "Sparkles"),
    ("⚡", "Lightning"),
    ("🌐", "Web"),
    ("📱", "Mobile"),
    ("🖥️", "Desktop"),
    ("⌨️", "Keyboard"),
    ("⚙️", "Gear"),
    ("🗄️", "Database"),
    ("☁️", "Cloud"),
    ("📦", "Package"),
    ("📚", "Books"),
    ("🧪", "Test tube"),
    ("🔒", "Lock"),
    ("🎮", "Game"),
    ("🎵", "Music"),
    ("🎬", "Movie"),
    ("🖼️", "Picture"),
    ("🛍️", "Shopping"),
    ("🔥", "Fire"),
    ("💡", "Idea"),
    ("🧩", "Puzzle"),
    ("📊", "Chart"),
    ("🧠", "Brain"),
    ("🦄", "Unicorn"),
    ("🐙", "Octopus"),
    ("🌱", "Seedling"),
];

/// `iconNames` of `lucide-react/dynamic`: every bundled Lucide icon, sorted.
pub fn lucide_icon_names() -> Vec<String> {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| {
            use gpui_kit::AssetSource as _;
            let mut names: Vec<String> = gpui_kit::assets::AllAssets
                .list("icons/")
                .unwrap_or_default()
                .into_iter()
                .filter_map(|path| {
                    path.strip_prefix("icons/")
                        .and_then(|name| name.strip_suffix(".svg"))
                        .map(str::to_owned)
                })
                .collect();
            names.sort();
            names
        })
        .clone()
}

/// `filterProjectIconNames` over `names`.
pub fn filter_project_icon_names_in(names: &[String], query: &str) -> Vec<String> {
    let normalized = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    if normalized.is_empty() {
        return POPULAR_PROJECT_ICONS
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
    }
    names
        .iter()
        .filter(|name| name.contains(&normalized))
        .take(60)
        .cloned()
        .collect()
}

/// `filterProjectIconNames`.
pub fn filter_project_icon_names(query: &str) -> Vec<String> {
    filter_project_icon_names_in(&lucide_icon_names(), query)
}

/// `\p{Extended_Pictographic}`: the pictographic blocks and the loose symbols
/// Unicode lists with them.
fn is_extended_pictographic(c: char) -> bool {
    matches!(
        c as u32,
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x2199
            | 0x21A9..=0x21AA
            | 0x231A..=0x231B
            | 0x2328
            | 0x2388
            | 0x23CF
            | 0x23E9..=0x23F3
            | 0x23F8..=0x23FA
            | 0x24C2
            | 0x25AA..=0x25AB
            | 0x25B6
            | 0x25C0
            | 0x25FB..=0x25FE
            | 0x2600..=0x2605
            | 0x2607..=0x2612
            | 0x2614..=0x2685
            | 0x2690..=0x2705
            | 0x2708..=0x2712
            | 0x2714
            | 0x2716
            | 0x271D
            | 0x2721
            | 0x2728
            | 0x2733..=0x2734
            | 0x2744
            | 0x2747
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2763..=0x2767
            | 0x2795..=0x2797
            | 0x27A1
            | 0x27B0
            | 0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B07
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1F0FF
            | 0x1F10D..=0x1F10F
            | 0x1F12F
            | 0x1F16C..=0x1F171
            | 0x1F17E..=0x1F17F
            | 0x1F18E
            | 0x1F191..=0x1F19A
            | 0x1F1AD..=0x1F1E5
            | 0x1F201..=0x1F20F
            | 0x1F21A
            | 0x1F22F
            | 0x1F232..=0x1F23A
            | 0x1F23C..=0x1F23F
            | 0x1F249..=0x1F3FA
            | 0x1F400..=0x1F53D
            | 0x1F546..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F774..=0x1F77F
            | 0x1F7D5..=0x1F7FF
            | 0x1F80C..=0x1F80F
            | 0x1F848..=0x1F84F
            | 0x1F85A..=0x1F85F
            | 0x1F888..=0x1F88F
            | 0x1F8AE..=0x1F8FF
            | 0x1F90C..=0x1F93A
            | 0x1F93C..=0x1F945
            | 0x1F947..=0x1FAFF
            | 0x1FC00..=0x1FFFD
    )
}

/// `firstEmoji`: the first complete grapheme when it is an emoji, a flag or a
/// keycap.
pub fn first_emoji(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let segment = trimmed.graphemes(true).next()?;
    let chars: Vec<char> = segment.chars().collect();
    let is_flag = chars.len() == 2
        && chars
            .iter()
            .all(|c| ('\u{1F1E6}'..='\u{1F1FF}').contains(c));
    let is_keycap = {
        let mut iter = chars.iter();
        matches!(iter.next(), Some('#' | '*' | '0'..='9')) && {
            let rest: Vec<&char> = iter.collect();
            matches!(rest.as_slice(), ['\u{FE0F}', '\u{20E3}'] | ['\u{20E3}'])
        }
    };
    if !chars.iter().any(|c| is_extended_pictographic(*c)) && !is_flag && !is_keycap {
        return None;
    }
    Some(segment.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_across_the_full_lucide_set() {
        assert!(filter_project_icon_names("alarm clock").contains(&"alarm-clock".to_owned()));
        assert!(filter_project_icon_names("alarm  \tclock").contains(&"alarm-clock".to_owned()));
    }

    #[test]
    fn shows_the_popular_icons_for_an_empty_query() {
        let names = filter_project_icon_names("  ");
        assert_eq!(names.first().map(String::as_str), Some("folder-code"));
        assert_eq!(names.len(), POPULAR_PROJECT_ICONS.len());
    }

    #[test]
    fn caps_the_search_at_sixty_results() {
        assert!(filter_project_icon_names("a").len() <= 60);
    }

    #[test]
    fn extracts_one_complete_emoji_grapheme() {
        assert_eq!(first_emoji("  👩🏽‍💻 hello").as_deref(), Some("👩🏽‍💻"));
        assert_eq!(first_emoji("🇺🇸 project").as_deref(), Some("🇺🇸"));
        assert_eq!(first_emoji("1️⃣ project").as_deref(), Some("1️⃣"));
        assert_eq!(first_emoji("plain text"), None);
    }

    #[test]
    fn maps_persisted_colors_to_theme_aware_values() {
        use crate::state::models::ProjectIconColor;
        let violet =
            crate::project_icon_colors::project_icon_color(ProjectIconColor::Violet, false);
        let violet_dark =
            crate::project_icon_colors::project_icon_color(ProjectIconColor::Violet, true);
        assert_eq!(
            violet,
            gpui_kit::Hsla::from(crate::theme::color::hex(0x7f22fe))
        );
        assert_eq!(
            violet_dark,
            gpui_kit::Hsla::from(crate::theme::color::hex(0xa684ff))
        );
    }

    #[test]
    fn every_emoji_in_the_grid_is_an_emoji() {
        for (emoji, _) in PROJECT_EMOJIS {
            assert_eq!(first_emoji(emoji).as_deref(), Some(*emoji));
        }
    }
}

//! Port of upstream/apps/web/src/projectIdentity.ts
//!
//! The generated monogram and color for a project without an icon.

use crate::components::sidebar::runtime::models::ProjectIconColor;
use crate::project_icon_colors::PROJECT_ICON_COLORS;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub monogram: String,
    pub color: ProjectIconColor,
}

/// `normalize("NFKC").trim()` (NFKC folding is not applied; see Deviations).
fn normalize_project_name(name: &str) -> &str {
    name.trim()
}

fn words(name: &str) -> Vec<&str> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect()
}

fn project_monogram(name: &str) -> String {
    let words = words(normalize_project_name(name));
    let Some(first_word) = words.first() else {
        return "PR".into();
    };
    let glyphs: Vec<char> = first_word.chars().collect();
    let first = glyphs[0];
    let second = glyphs[1..]
        .iter()
        .copied()
        .find(|c| c.is_numeric())
        .or_else(|| {
            if words.len() > 1 {
                words.last().and_then(|w| w.chars().next())
            } else {
                glyphs.last().copied()
            }
        })
        .unwrap_or(first);
    format!("{first}{second}")
        .to_uppercase()
        .chars()
        .take(2)
        .collect()
}

fn project_color(name: &str) -> ProjectIconColor {
    let lowered = normalize_project_name(name).to_lowercase();
    let seed = if lowered.is_empty() {
        "project".to_string()
    } else {
        lowered
    };
    let len = PROJECT_ICON_COLORS.len() as u64;
    let mut index: u64 = 0;
    for glyph in seed.chars() {
        index = (index * 31 + glyph as u64) % len;
    }
    PROJECT_ICON_COLORS[index as usize].value
}

/// `deriveProjectIdentity`.
pub fn derive_project_identity(name: &str) -> ProjectIdentity {
    ProjectIdentity {
        monogram: project_monogram(name),
        color: project_color(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monograms() {
        assert_eq!(project_monogram("turin"), "TN");
        assert_eq!(project_monogram("t3code"), "T3");
        assert_eq!(project_monogram("My Project"), "MP");
        assert_eq!(project_monogram("   "), "PR");
        assert_eq!(project_monogram("a"), "AA");
    }

    #[test]
    fn colors_are_stable() {
        assert_eq!(project_color("turin"), project_color("  Turin "));
        assert_eq!(project_color(""), project_color("project"));
    }
}

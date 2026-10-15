//! Port of `newProjectFolderName` from upstream/packages/shared/src/path.ts.
//! Lives here until the Rust `shared` crate ports `path.ts`.

use unicode_normalization::UnicodeNormalization;

// Windows refuses these as file names, with or without an extension.
// The upstream regex `^(con|prn|aux|nul|com[1-9]|lpt[1-9])$`, expanded.
const WINDOWS_RESERVED_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Folder name for a project started from just a name ("Pinball Stats" becomes
/// "pinball-stats"). Falls back to "project" when nothing usable is left.
pub fn new_project_folder_name(name: &str) -> String {
    let folded: String = name
        .nfkd()
        .filter(|ch| !is_combining_mark(*ch))
        .collect::<String>()
        .to_lowercase();
    let mut slug = String::new();
    let mut in_separator = false;
    for ch in folded.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            slug.push(ch);
            in_separator = false;
        } else if !in_separator {
            slug.push('-');
            in_separator = true;
        }
    }
    let slug = slug.trim_start_matches('-');
    let slug: String = slug.chars().take(64).collect();
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        return "project".to_owned();
    }
    if WINDOWS_RESERVED_NAMES.contains(&slug) {
        format!("{slug}-project")
    } else {
        slug.to_owned()
    }
}

/// Combining diacritical marks, U+0300 through U+036F, as upstream strips them.
fn is_combining_mark(ch: char) -> bool {
    ('\u{0300}'..='\u{036f}').contains(&ch)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::new_project_folder_name;

    #[test]
    fn names_a_new_projects_folder_from_any_typed_name() {
        assert_eq!(new_project_folder_name("Pinball Stats"), "pinball-stats");
        assert_eq!(new_project_folder_name("  Café & Crème!  "), "cafe-creme");
        assert_eq!(new_project_folder_name("../../etc"), "etc");
        // Nothing usable left, so the server falls back to a fixed name.
        assert_eq!(new_project_folder_name("🎱🎱"), "project");
        assert_eq!(
            new_project_folder_name(&format!("{} b", "a".repeat(63))),
            "a".repeat(63)
        );
    }

    #[test]
    fn avoids_windows_device_names() {
        assert_eq!(new_project_folder_name("Con"), "con-project");
        assert_eq!(new_project_folder_name("LPT1"), "lpt1-project");
        assert_eq!(new_project_folder_name("console"), "console");
    }
}

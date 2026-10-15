//! `serializeComposerFileLink` from upstream/packages/shared/src/composerTrigger.ts
//! (copied from the shared-crate port until the app depends on it).

use crate::composer::js_string::encode_uri;

fn basename(path: &str) -> &str {
    match path.rfind('/').max(path.rfind('\\')) {
        Some(sep) => &path[sep + 1..],
        None => path,
    }
}

fn escape_markdown_link_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]")
}

fn encode_markdown_link_destination(path: &str) -> String {
    encode_uri(path)
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('#', "%23")
        .replace('?', "%3F")
        .replace('\\', "%5C")
}

/// `[basename](encoded/path)`: the canonical text a mention chip serializes to.
pub fn serialize_composer_file_link(path: &str) -> String {
    let label = escape_markdown_link_label(basename(path));
    format!("[{label}]({})", encode_markdown_link_destination(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_basename_as_the_markdown_label() {
        assert_eq!(serialize_composer_file_link("path/to/package.json"), "[package.json](path/to/package.json)");
    }

    #[test]
    fn encodes_markdown_sensitive_destination_characters() {
        assert_eq!(
            serialize_composer_file_link("docs/My File (draft).md"),
            "[My File (draft).md](docs/My%20File%20%28draft%29.md)"
        );
    }

    #[test]
    fn supports_windows_paths() {
        assert_eq!(serialize_composer_file_link("C:\\repo\\src\\index.ts"), "[index.ts](C:%5Crepo%5Csrc%5Cindex.ts)");
    }

    #[test]
    fn preserves_paths_that_start_with_an_at_sign() {
        assert_eq!(serialize_composer_file_link("@scope/package.json"), "[package.json](@scope/package.json)");
    }
}

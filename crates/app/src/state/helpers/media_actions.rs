//! Port of upstream/packages/client-runtime/src/mediaActions.ts

/// Menu action ids shared by every client so labels and handlers line up across surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaActionId {
    CopyFullPath,
    CopyRelativePath,
    CopyUrl,
    OpenFile,
    Save,
    CopyImage,
}

impl MediaActionId {
    /// The id upstream clients use on the wire and in menus.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CopyFullPath => "copy-full-path",
            Self::CopyRelativePath => "copy-relative-path",
            Self::CopyUrl => "copy-url",
            Self::OpenFile => "open-file",
            Self::Save => "save",
            Self::CopyImage => "copy-image",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MediaActionId;

    #[test]
    fn ids_match_upstream() {
        assert_eq!(MediaActionId::CopyFullPath.as_str(), "copy-full-path");
        assert_eq!(MediaActionId::CopyImage.as_str(), "copy-image");
    }
}

//! Port of upstream/apps/web/src/modelPickerVisibility.ts
//!
//! The mounted picker is the source of truth. The document lookup is the
//! caller's `picker_mounted` flag: there is no DOM in the port.

pub fn is_model_picker_open(document_available: bool, picker_mounted: bool) -> bool {
    document_available && picker_mounted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_only_when_the_document_has_the_mounted_popover() {
        assert!(!is_model_picker_open(false, true));
        assert!(!is_model_picker_open(true, false));
        assert!(is_model_picker_open(true, true));
    }
}

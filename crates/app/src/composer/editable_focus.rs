//! Port of upstream/apps/web/src/lib/editableFocus.ts
//!
//! A focused descendant inside an open shadow root is walked before the
//! editable check. The DOM `closest` walk is the node itself plus ancestors.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableKind {
    Input,
    Textarea,
    Select,
    ContentEditableEmpty,
    ContentEditableTrue,
    ContentEditablePlain,
    Textbox,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusNode {
    pub kind: EditableKind,
    pub shadow_active: Option<Box<FocusNode>>,
    pub parent_editable: bool,
}

pub fn is_editable_focused(target: Option<&FocusNode>) -> bool {
    let mut element = target;
    while let Some(node) = element {
        if let Some(active) = node.shadow_active.as_deref() {
            element = Some(active);
            continue;
        }
        return node.kind != EditableKind::Other || node.parent_editable;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_a_text_field_focused_inside_an_open_shadow_root() {
        let mut host = FocusNode {
            kind: EditableKind::Other,
            shadow_active: None,
            parent_editable: false,
        };
        assert!(!is_editable_focused(Some(&host)));
        host.shadow_active = Some(Box::new(FocusNode {
            kind: EditableKind::Input,
            shadow_active: None,
            parent_editable: false,
        }));
        assert!(is_editable_focused(Some(&host)));
    }
}

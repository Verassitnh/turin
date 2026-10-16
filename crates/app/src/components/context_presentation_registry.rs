//! Port of upstream/apps/web/src/components/contextPresentationRegistry.ts
//!
//! Shared capability table for composer context chips, plus a checked
//! dispatcher each surface fills in with its own renderers. The views that
//! call this are not ported yet, so the binary does not reference it.
#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::OnceLock;

use turin_contracts::composer_context::COMPOSER_CONTEXT_KINDS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextDetails {
    None,
    Tooltip,
    Popover,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextExpanded {
    None,
    InlineBlock,
    Modal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextDraftView {
    Compact,
    Expanded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPresentationCapability {
    pub details: ContextDetails,
    pub expanded: ContextExpanded,
    pub default_draft_view: ContextDraftView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPresentationDefinition {
    pub kind: &'static str,
    pub capabilities: ContextPresentationCapability,
}

const DEFINITIONS: &[ContextPresentationDefinition] = &[
    ContextPresentationDefinition {
        kind: "image",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Tooltip,
            expanded: ContextExpanded::Modal,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "file",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Tooltip,
            expanded: ContextExpanded::Modal,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "terminal",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Popover,
            expanded: ContextExpanded::None,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "element",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Popover,
            expanded: ContextExpanded::None,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "preview-annotation",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Popover,
            expanded: ContextExpanded::Modal,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "review-comment",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Popover,
            expanded: ContextExpanded::None,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "mention",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::None,
            expanded: ContextExpanded::None,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "skill",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Tooltip,
            expanded: ContextExpanded::None,
            default_draft_view: ContextDraftView::Compact,
        },
    },
    ContextPresentationDefinition {
        kind: "thread",
        capabilities: ContextPresentationCapability {
            details: ContextDetails::Tooltip,
            expanded: ContextExpanded::None,
            default_draft_view: ContextDraftView::Compact,
        },
    },
];

fn validate_definitions() -> Result<(), String> {
    let mut seen = HashSet::new();
    for definition in DEFINITIONS {
        if !seen.insert(definition.kind) {
            return Err(format!(
                "Duplicate context presentation definition: {}",
                definition.kind
            ));
        }
    }
    for kind in COMPOSER_CONTEXT_KINDS {
        if !seen.contains(kind) {
            return Err(format!("Missing context presentation definition: {kind}"));
        }
    }
    Ok(())
}

pub fn context_presentation_definitions() -> &'static [ContextPresentationDefinition] {
    static VALID: OnceLock<Result<(), String>> = OnceLock::new();
    match VALID.get_or_init(validate_definitions) {
        Ok(()) => DEFINITIONS,
        Err(error) => panic!("{error}"),
    }
}

pub fn context_presentation_definition(kind: &str) -> &'static ContextPresentationDefinition {
    context_presentation_definitions()
        .iter()
        .find(|definition| definition.kind == kind)
        .unwrap_or_else(|| panic!("Missing context presentation definition: {kind}"))
}

pub struct ContextPresentationHandler<R, C, T> {
    pub kind: &'static str,
    pub can_render: Option<fn(&R, &C) -> bool>,
    pub render: fn(&R, &C, &ContextPresentationDefinition) -> T,
}

pub struct ContextPresentationRegistry<R, C, T> {
    handlers: Vec<ContextPresentationHandler<R, C, T>>,
    fallback: fn(&str, Option<&R>, &C) -> T,
}

pub fn create_context_presentation_registry<R, C, T>(
    handlers: Vec<ContextPresentationHandler<R, C, T>>,
    required_kinds: &[&str],
    fallback: fn(&str, Option<&R>, &C) -> T,
) -> Result<ContextPresentationRegistry<R, C, T>, String> {
    let mut seen = HashSet::new();
    for handler in &handlers {
        if !seen.insert(handler.kind) {
            return Err(format!(
                "Duplicate context presentation handler: {}",
                handler.kind
            ));
        }
    }
    for kind in required_kinds {
        if !seen.contains(kind) {
            return Err(format!("Missing context presentation handler: {kind}"));
        }
    }
    Ok(ContextPresentationRegistry { handlers, fallback })
}

impl<R, C, T> ContextPresentationRegistry<R, C, T> {
    pub fn definition(&self, kind: &str) -> &'static ContextPresentationDefinition {
        context_presentation_definition(kind)
    }

    pub fn render(&self, kind: &str, record: Option<&R>, context: &C) -> T {
        let Some(handler) = self.handlers.iter().find(|handler| handler.kind == kind) else {
            return (self.fallback)(kind, record, context);
        };
        let Some(record) = record else {
            return (self.fallback)(kind, None, context);
        };
        if handler
            .can_render
            .is_some_and(|can_render| !can_render(record, context))
        {
            return (self.fallback)(kind, Some(record), context);
        }
        (handler.render)(
            record,
            context,
            context_presentation_definition(handler.kind),
        )
    }
}

#[cfg(test)]
#[path = "context_presentation_registry_tests.rs"]
mod tests;

//! Port of upstream/apps/web/src/components/contextPresentationRegistry.test.ts

use super::*;

fn must_err<T, E>(result: Result<T, E>) -> E {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error,
    }
}

#[test]
fn defines_presentation_capabilities_for_every_known_context_kind() {
    let kinds: Vec<&str> = context_presentation_definitions()
        .iter()
        .map(|definition| definition.kind)
        .collect();
    assert_eq!(kinds, COMPOSER_CONTEXT_KINDS);
    assert_eq!(
        context_presentation_definition("terminal").capabilities,
        ContextPresentationCapability {
            details: ContextDetails::Popover,
            expanded: ContextExpanded::None,
            default_draft_view: ContextDraftView::Compact,
        }
    );
}

#[test]
fn rejects_duplicate_handlers_for_one_surface() {
    let error = must_err(create_context_presentation_registry(
        vec![
            ContextPresentationHandler {
                kind: "terminal",
                can_render: None,
                render: |_: &String, _: &(), _| "first".to_owned(),
            },
            ContextPresentationHandler {
                kind: "terminal",
                can_render: None,
                render: |_: &String, _: &(), _| "second".to_owned(),
            },
        ],
        &[],
        |_, _, _| "fallback".to_owned(),
    ));
    assert_eq!(error, "Duplicate context presentation handler: terminal");
}

#[test]
fn rejects_a_missing_handler_required_by_a_surface() {
    let error = must_err(create_context_presentation_registry(
        vec![ContextPresentationHandler {
            kind: "terminal",
            can_render: None,
            render: |_: &String, _: &(), _| "terminal".to_owned(),
        }],
        &["terminal", "review-comment"],
        |_, _, _| "fallback".to_owned(),
    ));
    assert_eq!(
        error,
        "Missing context presentation handler: review-comment"
    );
}

#[test]
fn uses_the_explicit_fallback_for_unknown_missing_and_mismatched_records() {
    let registry = create_context_presentation_registry(
        vec![ContextPresentationHandler {
            kind: "terminal",
            can_render: Some(|record: &String, _| record == "terminal"),
            render: |_, _, _| "rendered".to_owned(),
        }],
        &[],
        |kind, _, _| format!("fallback:{kind}"),
    )
    .unwrap();

    assert_eq!(registry.definition("terminal").kind, "terminal");
    let terminal = "terminal".to_owned();
    let file = "file".to_owned();
    let payload = "payload".to_owned();
    assert_eq!(
        registry.render("terminal", Some(&terminal), &()),
        "rendered"
    );
    assert_eq!(
        registry.render("terminal", Some(&file), &()),
        "fallback:terminal"
    );
    assert_eq!(registry.render("terminal", None, &()), "fallback:terminal");
    assert_eq!(
        registry.render("future-kind", Some(&payload), &()),
        "fallback:future-kind"
    );
}

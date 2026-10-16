//! Port of upstream/packages/client-runtime/src/errors/safeLog.test.ts

use super::*;
use crate::state::errors::error_graph::ErrorNode;

fn diagnostic_text(attributes: &SafeErrorLogAttributes) -> String {
    [
        Some(attributes.error_type.as_str().to_owned()),
        attributes.error_name.clone(),
        attributes.error_tag.clone(),
        attributes.trace_id.clone(),
        attributes.stack.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn keeps_correlation_and_stack_frames_without_serializing_messages_or_nested_causes() {
    // Messages are not part of the graph at all; the sentinels live in the
    // stack's header line, which the frame filter must drop.
    let mut graph = ErrorGraph::default();
    let cause = graph.add(ErrorNode::error("Error").with_trace_id("trace-safe-123"));
    let error = graph.add(
        ErrorNode::error("Error")
            .with_tag("ProjectRemovalError")
            .with_cause(Some(cause))
            .with_stack(
                "ProjectRemovalError: outer-error-secret-sentinel\n    at removeProject (https://user:password@example.com/project.ts?token=secret#fragment)",
            ),
    );

    let attributes = safe_error_log_attributes(&graph, Some(error));

    assert_eq!(attributes.error_type, SafeErrorType::Error);
    assert_eq!(attributes.error_name.as_deref(), Some("Error"));
    assert_eq!(attributes.error_tag.as_deref(), Some("ProjectRemovalError"));
    assert_eq!(attributes.trace_id.as_deref(), Some("trace-safe-123"));
    assert_eq!(
        attributes.stack.as_deref(),
        Some("    at removeProject (https://example.com/project.ts)")
    );
    let text = diagnostic_text(&attributes);
    assert!(!text.contains("outer-error-secret-sentinel"));
    assert!(!text.contains("nested-cause-secret-sentinel"));
    assert!(!text.contains("user:password"));
    assert!(!text.contains("token=secret"));
}

#[test]
fn does_not_trust_arbitrary_object_messages_or_tags() {
    let mut graph = ErrorGraph::default();
    let cause = graph.add(ErrorNode::object().with_trace_id("trace id with unsafe whitespace"));
    let error = graph.add(
        ErrorNode::object()
            .with_tag("payload-secret-sentinel")
            .with_cause(Some(cause)),
    );

    assert_eq!(
        safe_error_log_attributes(&graph, Some(error)),
        SafeErrorLogAttributes {
            error_type: SafeErrorType::Object,
            error_name: None,
            error_tag: None,
            trace_id: None,
            stack: None,
        }
    );
}

#[test]
fn skips_an_unsafe_outer_trace_id_when_a_nested_safe_trace_id_is_available() {
    let mut graph = ErrorGraph::default();
    let inner = graph.add(ErrorNode::object().with_trace_id("trace-safe-inner"));
    let outer = graph.add(
        ErrorNode::object()
            .with_trace_id("unsafe trace id")
            .with_cause(Some(inner)),
    );

    assert_eq!(
        safe_error_log_attributes(&graph, Some(outer)),
        SafeErrorLogAttributes {
            error_type: SafeErrorType::Object,
            error_name: None,
            error_tag: None,
            trace_id: Some("trace-safe-inner".to_owned()),
            stack: None,
        }
    );
}

#[test]
fn classifies_non_error_values_and_survives_cyclic_causes() {
    let mut graph = ErrorGraph::default();
    let null = graph.add(ErrorNode::new(ErrorKind::Null));
    let array = graph.add(ErrorNode::new(ErrorKind::Array));
    let primitive = graph.add(ErrorNode::new(ErrorKind::Primitive));
    let cyclic = graph.add(ErrorNode::object());
    graph.set_cause(cyclic, Some(cyclic));

    let kind = |id| safe_error_log_attributes(&graph, id).error_type;
    assert_eq!(kind(Some(null)), SafeErrorType::Null);
    assert_eq!(kind(Some(array)), SafeErrorType::Array);
    assert_eq!(kind(Some(primitive)), SafeErrorType::Primitive);
    assert_eq!(kind(None), SafeErrorType::Primitive);
    assert_eq!(
        safe_error_log_attributes(&graph, Some(cyclic)).trace_id,
        None
    );
}

#[test]
fn unsafe_error_names_are_dropped_and_stack_frames_are_limited() {
    let mut graph = ErrorGraph::default();
    let frames: Vec<String> = (0..40)
        .map(|index| format!("    at frame{index} (file:///a.ts)"))
        .collect();
    let error =
        graph.add(ErrorNode::error("secret name with spaces").with_stack(&frames.join("\r\n")));
    let attributes = safe_error_log_attributes(&graph, Some(error));
    assert_eq!(attributes.error_name, None);
    assert_eq!(attributes.stack.unwrap().lines().count(), 32);

    let no_frames = graph.add(ErrorNode::error("TypeError").with_stack("TypeError: nope"));
    assert_eq!(
        safe_error_log_attributes(&graph, Some(no_frames)).stack,
        None
    );

    let browser = graph
        .add(ErrorNode::error("RangeError").with_stack("run@https://x.test/a.js?q=1:3:4\nnoise"));
    // The URL pattern runs to the closing paren or whitespace, so the query
    // strip takes the `:3:4` position suffix with it, as upstream does.
    assert_eq!(
        safe_error_log_attributes(&graph, Some(browser))
            .stack
            .as_deref(),
        Some("run@https://x.test/a.js")
    );
}

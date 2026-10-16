//! Port of upstream/packages/client-runtime/src/errors/errorTrace.test.ts

use super::*;
use crate::state::errors::error_graph::ErrorNode;

#[test]
fn finds_trace_metadata_through_wrapped_typed_errors() {
    let mut graph = ErrorGraph::default();
    let inner = graph.add(
        ErrorNode::object()
            .with_tag("RelayInternalError")
            .with_trace_id("trace-relay"),
    );
    let middle = graph.add(ErrorNode::object().with_cause(Some(inner)));
    let outer = graph.add(ErrorNode::object().with_cause(Some(middle)));

    assert_eq!(
        find_error_trace_id(&graph, Some(outer)),
        Some("trace-relay".to_owned())
    );
}

#[test]
fn terminates_for_cyclic_causes() {
    let mut graph = ErrorGraph::default();
    let error = graph.add(ErrorNode::object());
    graph.set_cause(error, Some(error));

    assert_eq!(find_error_trace_id(&graph, Some(error)), None);
}

#[test]
fn finds_trace_metadata_in_effect_cause_branches() {
    let mut graph = ErrorGraph::default();
    let first = graph.add(ErrorNode::error("Error"));
    let second = graph.add(ErrorNode::object().with_trace_id("trace-secondary"));
    let cause = graph.add(ErrorNode::new(ErrorKind::Cause(vec![
        CauseReason::Fail(Some(first)),
        CauseReason::Fail(Some(second)),
    ])));

    assert_eq!(
        find_error_trace_id(&graph, Some(cause)),
        Some("trace-secondary".to_owned())
    );
}

#[test]
fn finds_trace_metadata_in_aggregate_error_branches() {
    let mut graph = ErrorGraph::default();
    let first = graph.add(ErrorNode::error("Error"));
    let second = graph.add(ErrorNode::object().with_trace_id("trace-aggregate"));
    let error =
        graph.add(ErrorNode::error("AggregateError").with_errors(vec![Some(first), Some(second)]));

    assert_eq!(
        find_error_trace_id(&graph, Some(error)),
        Some("trace-aggregate".to_owned())
    );
}

#[test]
fn blank_trace_ids_are_skipped_and_the_node_budget_is_bounded() {
    let mut graph = ErrorGraph::default();
    let inner = graph.add(ErrorNode::object().with_trace_id("trace-inner"));
    let blank = graph.add(
        ErrorNode::object()
            .with_trace_id("  \n")
            .with_cause(Some(inner)),
    );
    assert_eq!(
        find_error_trace_id(&graph, Some(blank)),
        Some("trace-inner".to_owned())
    );

    // 128 inspected nodes: a trace id past the budget is never found.
    let mut deep = graph.add(ErrorNode::object().with_trace_id("too-deep"));
    for _ in 0..MAX_ERROR_TRACE_NODES {
        deep = graph.add(ErrorNode::object().with_cause(Some(deep)));
    }
    assert_eq!(find_error_trace_id(&graph, Some(deep)), None);
    assert_eq!(find_error_trace_id(&graph, None), None);
}

//! Port of upstream/packages/client-runtime/src/errors/errorTrace.ts

use std::collections::HashSet;

use super::error_graph::{CauseReason, ErrorGraph, ErrorKind, Link};

const MAX_ERROR_TRACE_NODES: usize = 128;

/// `findErrorTraceId`: depth-first through `errors`, Effect cause reasons and
/// `cause`, inspecting at most 128 nodes and visiting each object once.
pub fn find_error_trace_id(graph: &ErrorGraph, error: Link) -> Option<String> {
    let mut seen = HashSet::new();
    let mut pending: Vec<Link> = vec![error];
    let mut inspected_node_count = 0;

    while inspected_node_count < MAX_ERROR_TRACE_NODES {
        let Some(current) = pending.pop() else { break };
        inspected_node_count += 1;
        let Some(id) = current else { continue };
        if !seen.insert(id) {
            continue;
        }
        let node = graph.get(id);
        if let Some(trace_id) = &node.trace_id
            && !js_trim_is_empty(trace_id)
        {
            return Some(trace_id.clone());
        }

        if let Some(errors) = &node.errors {
            pending.extend(errors.iter().rev().copied());
        }
        if let ErrorKind::Cause(reasons) = &node.kind {
            for reason in reasons.iter().rev() {
                match reason {
                    CauseReason::Fail(error) => pending.push(*error),
                    CauseReason::Die(defect) => pending.push(*defect),
                    CauseReason::Interrupt => {}
                }
            }
        }
        if let Some(cause) = node.cause {
            pending.push(cause);
        }
    }

    None
}

fn js_trim_is_empty(value: &str) -> bool {
    turin_contracts::base_schemas::js_trim(value).is_empty()
}

#[cfg(test)]
#[path = "error_trace_tests.rs"]
mod tests;

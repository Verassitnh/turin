//! Port of upstream/packages/client-runtime/src/errors/safeLog.ts
//!
//! Log attributes that keep correlation (`_tag`, `traceId`, the error name)
//! and sanitized stack frames, and never the message or nested causes.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use super::error_graph::{ErrorGraph, ErrorKind, Link};

static SAFE_ERROR_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:Error|EvalError|RangeError|ReferenceError|SyntaxError|TypeError|URIError|AggregateError|DOMException|[A-Za-z][A-Za-z0-9]*(?:Error|Failure))$",
    )
    .expect("safe error label pattern")
});
static SAFE_TRACE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._:-]{1,128}$").expect("safe trace id pattern"));
static STACK_URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:https?|file)://[^\s)]+").expect("stack url pattern"));
static AT_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*at\s+").expect("at frame pattern"));
static AT_URL_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^@\s]+@(?:https?|file)://").expect("at-url frame pattern"));
const STACK_FRAME_LIMIT: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeErrorType {
    Error,
    Array,
    Null,
    Object,
    Primitive,
}

impl SafeErrorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Array => "array",
            Self::Null => "null",
            Self::Object => "object",
            Self::Primitive => "primitive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeErrorLogAttributes {
    pub error_type: SafeErrorType,
    pub error_name: Option<String>,
    pub error_tag: Option<String>,
    pub trace_id: Option<String>,
    pub stack: Option<String>,
}

fn read_safe_label(value: &str) -> Option<String> {
    SAFE_ERROR_LABEL.is_match(value).then(|| value.to_owned())
}

/// `sanitizeStackUrl`: drops credentials, query and fragment; anything that
/// does not parse as a URL is left as it is.
fn sanitize_stack_url(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn sanitize_stack_frame(frame: &str) -> String {
    STACK_URL
        .replace_all(frame, |captures: &regex::Captures<'_>| {
            sanitize_stack_url(&captures[0])
        })
        .into_owned()
}

fn read_safe_stack(stack: Option<&str>) -> Option<String> {
    let frames: Vec<String> = stack?
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| AT_FRAME.is_match(line) || AT_URL_FRAME.is_match(line))
        .take(STACK_FRAME_LIMIT)
        .map(sanitize_stack_frame)
        .collect();
    (!frames.is_empty()).then(|| frames.join("\n"))
}

fn read_trace_id(graph: &ErrorGraph, error: Link) -> Option<String> {
    let mut seen = HashSet::new();
    let mut current = error;
    while let Some(id) = current {
        if !seen.insert(id) {
            break;
        }
        let node = graph.get(id);
        if let Some(trace_id) = &node.trace_id
            && SAFE_TRACE_ID.is_match(trace_id)
        {
            return Some(trace_id.clone());
        }
        current = node.cause.flatten();
    }
    None
}

/// `safeErrorLogAttributes`.
pub fn safe_error_log_attributes(graph: &ErrorGraph, error: Link) -> SafeErrorLogAttributes {
    let node = error.map(|id| graph.get(id));
    let error_tag = node
        .and_then(|node| node.tag.as_deref())
        .and_then(read_safe_label);
    let trace_id = read_trace_id(graph, error);

    if let Some(node) = node
        && let ErrorKind::Error { name, stack } = &node.kind
    {
        return SafeErrorLogAttributes {
            error_type: SafeErrorType::Error,
            error_name: read_safe_label(name),
            error_tag,
            trace_id,
            stack: read_safe_stack(stack.as_deref()),
        };
    }

    let error_type = match node.map(|node| &node.kind) {
        Some(ErrorKind::Null) => SafeErrorType::Null,
        Some(ErrorKind::Array) => SafeErrorType::Array,
        Some(ErrorKind::Object | ErrorKind::Cause(_)) => SafeErrorType::Object,
        // `undefined` and other non-objects are `typeof` primitives.
        None | Some(ErrorKind::Primitive) => SafeErrorType::Primitive,
        Some(ErrorKind::Error { .. }) => unreachable!("handled above"),
    };
    SafeErrorLogAttributes {
        error_type,
        error_name: None,
        error_tag,
        trace_id,
        stack: None,
    }
}

#[cfg(test)]
#[path = "safe_log_tests.rs"]
mod tests;

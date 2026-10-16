//! The JS error-shaped values `errorTrace.ts` and `safeLog.ts` inspect.
//!
//! Upstream walks `unknown` by property. A node here has exactly the
//! properties those walks read, and a [`Link`] is either another node or a
//! value that is not an object (a string, `undefined`, `null`), which the
//! walks count and skip. Nodes are addressed by [`ErrorId`] so a node can be
//! its own `cause`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorId(usize);

/// A property value: another object, or something the walks skip.
pub type Link = Option<ErrorId>;

/// `typeof`/`Array.isArray`/`instanceof Error` classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// `instanceof Error`, with its `name` and `stack`.
    Error {
        name: String,
        stack: Option<String>,
    },
    Object,
    Array,
    Null,
    Primitive,
    /// An Effect `Cause`: its reasons are read by `findErrorTraceId`.
    Cause(Vec<CauseReason>),
}

/// A `Cause` reason; `Interrupt` carries nothing the walk reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CauseReason {
    Fail(Link),
    Die(Link),
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorNode {
    pub kind: ErrorKind,
    /// `_tag`, when it is a string.
    pub tag: Option<String>,
    /// `traceId`, when it is a string.
    pub trace_id: Option<String>,
    /// `"cause" in record`: `Some(link)` when the property exists.
    pub cause: Option<Link>,
    /// `Array.isArray(record.errors)`: the elements, when it is an array.
    pub errors: Option<Vec<Link>>,
}

impl ErrorNode {
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            tag: None,
            trace_id: None,
            cause: None,
            errors: None,
        }
    }

    pub fn object() -> Self {
        Self::new(ErrorKind::Object)
    }

    pub fn error(name: &str) -> Self {
        Self::new(ErrorKind::Error {
            name: name.to_owned(),
            stack: None,
        })
    }

    pub fn with_tag(mut self, tag: &str) -> Self {
        self.tag = Some(tag.to_owned());
        self
    }

    pub fn with_trace_id(mut self, trace_id: &str) -> Self {
        self.trace_id = Some(trace_id.to_owned());
        self
    }

    pub fn with_cause(mut self, cause: Link) -> Self {
        self.cause = Some(cause);
        self
    }

    pub fn with_errors(mut self, errors: Vec<Link>) -> Self {
        self.errors = Some(errors);
        self
    }

    pub fn with_stack(mut self, stack: &str) -> Self {
        if let ErrorKind::Error { stack: slot, .. } = &mut self.kind {
            *slot = Some(stack.to_owned());
        }
        self
    }
}

#[derive(Debug, Default)]
pub struct ErrorGraph {
    nodes: Vec<ErrorNode>,
}

impl ErrorGraph {
    pub fn add(&mut self, node: ErrorNode) -> ErrorId {
        self.nodes.push(node);
        ErrorId(self.nodes.len() - 1)
    }

    pub fn get(&self, id: ErrorId) -> &ErrorNode {
        &self.nodes[id.0]
    }

    /// Points `id.cause` at `cause`, for cycles built after the fact.
    pub fn set_cause(&mut self, id: ErrorId, cause: Link) {
        self.nodes[id.0].cause = Some(cause);
    }
}

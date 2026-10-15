//! Port of upstream/packages/provider-core/src/server/IdAllocator.ts
//! (+ `randomUuid.ts`).
//!
//! Upstream has one implementation (`IdAllocatorV2.layer`), so this is a plain
//! struct. The allocation methods are infallible here: upstream's
//! `IdAllocatorV2AllocationError` only wrapped `Random` failures, which cannot
//! happen with `uuid::Uuid::new_v4` (see PORTING.md Deviations).

use turin_contracts::{
    CheckpointId, CheckpointScopeId, CommandId, ContextHandoffId, ContextTransferId, EventId,
    MessageId, NodeId, PlanId, ProjectId, ProviderSessionId, ProviderThreadId, ProviderTurnId,
    RawEventId, RunAttemptId, RunId, RuntimeRequestId, ThreadId, TurnItemId,
    provider_instance::{ProviderDriverKind, ProviderInstanceId},
};
use uuid::Uuid;

/// A path-like part of an id: strings and ordinals, as upstream's
/// `string | number` parts.
#[derive(Clone, Copy)]
pub enum Part<'a> {
    Str(&'a str),
    Num(u64),
}

impl<'a> From<&'a str> for Part<'a> {
    fn from(value: &'a str) -> Self {
        Self::Str(value)
    }
}

impl<'a> From<&'a String> for Part<'a> {
    fn from(value: &'a String) -> Self {
        Self::Str(value)
    }
}

impl From<u64> for Part<'_> {
    fn from(value: u64) -> Self {
        Self::Num(value)
    }
}

/// `encodeURIComponent`: everything except `A-Z a-z 0-9 - _ . ! ~ * ' ( )`.
pub fn encode_uri_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn encode_part(part: &Part<'_>) -> String {
    match part {
        Part::Str(value) => encode_uri_component(value),
        Part::Num(value) => value.to_string(),
    }
}

fn join_id(prefix: &str, parts: &[Part<'_>]) -> String {
    let mut out = String::from(prefix);
    for part in parts {
        out.push(':');
        out.push_str(&encode_part(part));
    }
    out
}

macro_rules! parts {
    ($($part:expr),* $(,)?) => {
        &[$(Part::from($part)),*]
    };
}

/// Ids built here always have a non-blank prefix, so construction cannot fail.
fn make<T, E: std::fmt::Debug>(build: fn(String) -> Result<T, E>, value: String) -> T {
    build(value).expect("derived ids are non-blank")
}

fn random_id(prefix: &str, parts: &[Part<'_>]) -> String {
    let uuid = Uuid::new_v4().to_string();
    let mut all: Vec<Part<'_>> = Vec::with_capacity(parts.len() + 1);
    all.extend_from_slice(parts);
    all.push(Part::Str(&uuid));
    join_id(prefix, &all)
}

/// Mints new ids; every call yields a fresh value (`allocate` upstream).
#[derive(Debug, Clone, Copy, Default)]
pub struct IdAllocator;

impl IdAllocator {
    pub fn command(&self, fixture_name: &str, command_name: &str) -> CommandId {
        make(
            CommandId::new,
            random_id("command", parts!["fixture", fixture_name, command_name]),
        )
    }

    pub fn event(
        &self,
        thread_id: Option<&ThreadId>,
        command_id: Option<&CommandId>,
        provider_session_id: Option<&ProviderSessionId>,
    ) -> EventId {
        let mut p: Vec<Part<'_>> = Vec::new();
        if let Some(thread_id) = thread_id {
            p.push("thread".into());
            p.push(thread_id.as_str().into());
        }
        if let Some(command_id) = command_id {
            p.push("command".into());
            p.push(command_id.as_str().into());
        }
        if let Some(provider_session_id) = provider_session_id {
            p.push("provider-session".into());
            p.push(provider_session_id.as_str().into());
        }
        make(EventId::new, random_id("event", &p))
    }

    pub fn raw_event(
        &self,
        provider_session_id: &ProviderSessionId,
        method: Option<&str>,
    ) -> RawEventId {
        let mut p: Vec<Part<'_>> = vec![
            "provider-session".into(),
            provider_session_id.as_str().into(),
        ];
        if let Some(method) = method {
            p.push("method".into());
            p.push(method.into());
        }
        make(RawEventId::new, random_id("raw-event", &p))
    }

    pub fn project(&self, fixture_name: &str) -> ProjectId {
        make(
            ProjectId::new,
            random_id("project", parts!["fixture", fixture_name]),
        )
    }

    pub fn thread(&self, fixture_name: Option<&str>, project_id: Option<&ProjectId>) -> ThreadId {
        let mut p: Vec<Part<'_>> = Vec::new();
        if let Some(fixture_name) = fixture_name {
            p.push("fixture".into());
            p.push(fixture_name.into());
        }
        if let Some(project_id) = project_id {
            p.push("project".into());
            p.push(project_id.as_str().into());
        }
        make(ThreadId::new, random_id("thread", &p))
    }

    pub fn message(&self, thread_id: &ThreadId, ordinal: u64) -> MessageId {
        make(
            MessageId::new,
            random_id(
                "message",
                parts!["thread", thread_id.as_str(), "ordinal", ordinal],
            ),
        )
    }

    pub fn provider_session(
        &self,
        provider_instance_id: &ProviderInstanceId,
        thread_id: &ThreadId,
    ) -> ProviderSessionId {
        make(
            ProviderSessionId::new,
            random_id(
                "provider-session",
                parts![
                    "provider-instance",
                    provider_instance_id.as_str(),
                    "thread",
                    thread_id.as_str()
                ],
            ),
        )
    }

    pub fn runtime_request(
        &self,
        driver: &ProviderDriverKind,
        provider_turn_id: Option<&ProviderTurnId>,
        native_request_id: Option<&str>,
    ) -> RuntimeRequestId {
        let mut p: Vec<Part<'_>> = vec!["provider".into(), driver.as_str().into()];
        if let Some(provider_turn_id) = provider_turn_id {
            p.push("provider-turn".into());
            p.push(provider_turn_id.as_str().into());
        }
        if let Some(native_request_id) = native_request_id {
            p.push("native-request".into());
            p.push(native_request_id.into());
        }
        make(RuntimeRequestId::new, random_id("runtime-request", &p))
    }

    /// Deterministic, as upstream (`checkpointScope` never used `randomId`).
    pub fn checkpoint_scope(&self, thread_id: &ThreadId, name: &str) -> CheckpointScopeId {
        make(
            CheckpointScopeId::new,
            join_id(
                "checkpoint-scope",
                parts!["thread", thread_id.as_str(), "name", name],
            ),
        )
    }

    /// Deterministic, as upstream.
    pub fn checkpoint(&self, checkpoint_scope_id: &CheckpointScopeId, name: &str) -> CheckpointId {
        make(
            CheckpointId::new,
            join_id(
                "checkpoint",
                parts!["scope", checkpoint_scope_id.as_str(), "name", name],
            ),
        )
    }

    pub fn context_handoff(
        &self,
        thread_id: &ThreadId,
        from_provider_instance_id: &ProviderInstanceId,
        to_provider_instance_id: &ProviderInstanceId,
    ) -> ContextHandoffId {
        make(
            ContextHandoffId::new,
            random_id(
                "context-handoff",
                parts![
                    "thread",
                    thread_id.as_str(),
                    "from-provider-instance",
                    from_provider_instance_id.as_str(),
                    "to-provider-instance",
                    to_provider_instance_id.as_str()
                ],
            ),
        )
    }

    pub fn context_transfer(
        &self,
        source_thread_id: &ThreadId,
        target_thread_id: &ThreadId,
        kind: &str,
    ) -> ContextTransferId {
        make(
            ContextTransferId::new,
            random_id(
                "context-transfer",
                parts![
                    "type",
                    kind,
                    "source-thread",
                    source_thread_id.as_str(),
                    "target-thread",
                    target_thread_id.as_str()
                ],
            ),
        )
    }

    pub fn plan(
        &self,
        thread_id: &ThreadId,
        run_id: Option<&RunId>,
        driver: &ProviderDriverKind,
    ) -> PlanId {
        let mut p: Vec<Part<'_>> = vec![
            "thread".into(),
            thread_id.as_str().into(),
            "provider".into(),
            driver.as_str().into(),
        ];
        if let Some(run_id) = run_id {
            p.push("run".into());
            p.push(run_id.as_str().into());
        }
        make(PlanId::new, random_id("plan", &p))
    }
}

/// Deterministic id derivations; pure, so callers that only derive need no
/// allocator (upstream `IdAllocatorV2Derive` / the `derive` export).
pub mod derive {
    use super::*;

    pub fn provider_session(provider_instance_id: &ProviderInstanceId) -> ProviderSessionId {
        make(
            ProviderSessionId::new,
            join_id(
                "provider-session",
                parts!["provider-instance", provider_instance_id.as_str(), "shared"],
            ),
        )
    }

    pub fn delegated_task_node(command_id: &CommandId) -> NodeId {
        make(
            NodeId::new,
            join_id("node", parts!["delegated-task", command_id.as_str()]),
        )
    }

    pub fn delegated_task_thread(command_id: &CommandId) -> ThreadId {
        make(
            ThreadId::new,
            join_id("thread", parts!["delegated-task", command_id.as_str()]),
        )
    }

    pub fn delegated_task_message(command_id: &CommandId) -> MessageId {
        make(
            MessageId::new,
            join_id("message", parts!["delegated-task", command_id.as_str()]),
        )
    }

    pub fn delegated_task_turn_item(command_id: &CommandId) -> TurnItemId {
        make(
            TurnItemId::new,
            join_id("turn-item", parts!["delegated-task", command_id.as_str()]),
        )
    }

    pub fn created_thread_turn_item(command_id: &CommandId) -> TurnItemId {
        make(
            TurnItemId::new,
            join_id("turn-item", parts!["created-thread", command_id.as_str()]),
        )
    }

    pub fn thread_from_provider_thread(
        driver: &ProviderDriverKind,
        provider_instance_id: Option<&ProviderInstanceId>,
        native_thread_id: &str,
    ) -> ThreadId {
        make(
            ThreadId::new,
            join_id(
                "thread",
                &provider_thread_parts(driver, provider_instance_id, native_thread_id),
            ),
        )
    }

    pub fn provider_thread(
        driver: &ProviderDriverKind,
        provider_instance_id: Option<&ProviderInstanceId>,
        native_thread_id: &str,
    ) -> ProviderThreadId {
        make(
            ProviderThreadId::new,
            join_id(
                "provider-thread",
                &provider_thread_parts(driver, provider_instance_id, native_thread_id),
            ),
        )
    }

    fn provider_thread_parts<'a>(
        driver: &'a ProviderDriverKind,
        provider_instance_id: Option<&'a ProviderInstanceId>,
        native_thread_id: &'a str,
    ) -> Vec<Part<'a>> {
        let mut parts: Vec<Part<'a>> = vec!["provider".into(), driver.as_str().into()];
        if let Some(provider_instance_id) = provider_instance_id {
            parts.push("provider-instance".into());
            parts.push(provider_instance_id.as_str().into());
        }
        parts.push("native-thread".into());
        parts.push(native_thread_id.into());
        parts
    }

    pub fn run(thread_id: &ThreadId, ordinal: u64) -> RunId {
        make(
            RunId::new,
            join_id(
                "run",
                parts!["thread", thread_id.as_str(), "ordinal", ordinal],
            ),
        )
    }

    pub fn run_attempt(run_id: &RunId, attempt_ordinal: u64) -> RunAttemptId {
        make(
            RunAttemptId::new,
            join_id(
                "run-attempt",
                parts!["run", run_id.as_str(), "attempt", attempt_ordinal],
            ),
        )
    }

    pub fn root_node(run_id: &RunId) -> NodeId {
        make(
            NodeId::new,
            join_id("node", parts!["run", run_id.as_str(), "root"]),
        )
    }

    pub fn root_node_attempt(run_id: &RunId, attempt_ordinal: u64) -> NodeId {
        make(
            NodeId::new,
            join_id(
                "node",
                parts!["run", run_id.as_str(), "attempt", attempt_ordinal, "root"],
            ),
        )
    }

    pub fn user_turn_item(message_id: &MessageId) -> TurnItemId {
        make(
            TurnItemId::new,
            join_id("turn-item", parts!["message", message_id.as_str()]),
        )
    }

    pub fn run_signal_turn_item(run_id: &RunId, signal: &str) -> TurnItemId {
        make(
            TurnItemId::new,
            join_id(
                "turn-item",
                parts!["run", run_id.as_str(), "signal", signal],
            ),
        )
    }

    pub fn provider_turn(driver: &ProviderDriverKind, native_turn_id: &str) -> ProviderTurnId {
        make(
            ProviderTurnId::new,
            join_id(
                "provider-turn",
                parts!["provider", driver.as_str(), "native-turn", native_turn_id],
            ),
        )
    }

    pub fn node_from_provider_item(driver: &ProviderDriverKind, native_item_id: &str) -> NodeId {
        make(
            NodeId::new,
            join_id(
                "node",
                parts!["provider", driver.as_str(), "native-item", native_item_id],
            ),
        )
    }

    pub fn message_from_provider_item(
        driver: &ProviderDriverKind,
        native_item_id: &str,
    ) -> MessageId {
        make(
            MessageId::new,
            join_id(
                "message",
                parts!["provider", driver.as_str(), "native-item", native_item_id],
            ),
        )
    }

    pub fn turn_item_from_provider_item(
        driver: &ProviderDriverKind,
        native_item_id: &str,
    ) -> TurnItemId {
        make(
            TurnItemId::new,
            join_id(
                "turn-item",
                parts!["provider", driver.as_str(), "native-item", native_item_id],
            ),
        )
    }

    pub fn approval_node(request_id: &RuntimeRequestId) -> NodeId {
        make(
            NodeId::new,
            join_id("node", parts!["runtime-request", request_id.as_str()]),
        )
    }

    pub fn approval_turn_item(request_id: &RuntimeRequestId) -> TurnItemId {
        make(
            TurnItemId::new,
            join_id("turn-item", parts!["runtime-request", request_id.as_str()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver() -> ProviderDriverKind {
        ProviderDriverKind::new("codex").unwrap()
    }

    #[test]
    fn encode_uri_component_matches_js() {
        assert_eq!(encode_uri_component("a b/c:d"), "a%20b%2Fc%3Ad");
        assert_eq!(encode_uri_component("é😀"), "%C3%A9%F0%9F%98%80");
        assert_eq!(encode_uri_component("A-z_0.!~*'()"), "A-z_0.!~*'()");
    }

    #[test]
    fn derives_deterministic_ids() {
        let thread = ThreadId::new("thread:a b").unwrap();
        let run = derive::run(&thread, 3);
        assert_eq!(run.as_str(), "run:thread:thread%3Aa%20b:ordinal:3");
        assert_eq!(
            derive::root_node_attempt(&run, 2).as_str(),
            format!(
                "node:run:{}:attempt:2:root",
                encode_uri_component(run.as_str())
            )
        );
        assert_eq!(
            derive::provider_turn(&driver(), "t1").as_str(),
            "provider-turn:provider:codex:native-turn:t1"
        );
        let instance = ProviderInstanceId::new("codex_work").unwrap();
        assert_eq!(
            derive::provider_thread(&driver(), Some(&instance), "n/1").as_str(),
            "provider-thread:provider:codex:provider-instance:codex_work:native-thread:n%2F1"
        );
        assert_eq!(
            derive::thread_from_provider_thread(&driver(), None, "n1").as_str(),
            "thread:provider:codex:native-thread:n1"
        );
        assert_eq!(
            derive::provider_session(&instance).as_str(),
            "provider-session:provider-instance:codex_work:shared"
        );
    }

    #[test]
    fn allocates_unique_random_ids_with_context_parts() {
        let ids = IdAllocator;
        let thread = ThreadId::new("thread-1").unwrap();
        let a = ids.message(&thread, 4);
        let b = ids.message(&thread, 4);
        assert_ne!(a, b);
        assert!(a.as_str().starts_with("message:thread:thread-1:ordinal:4:"));
        let scope = ids.checkpoint_scope(&thread, "main");
        assert_eq!(scope.as_str(), "checkpoint-scope:thread:thread-1:name:main");
        assert_eq!(
            ids.checkpoint(&scope, "c1").as_str(),
            "checkpoint:scope:checkpoint-scope%3Athread%3Athread-1%3Aname%3Amain:name:c1"
        );
        let event = ids.event(Some(&thread), None, None);
        assert!(event.as_str().starts_with("event:thread:thread-1:"));
    }
}

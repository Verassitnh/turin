//! Port of the control-plane reads in `ProjectionStore.ts`: the rows a provider
//! or runtime-response handler reads before it acts. SQL is copied verbatim
//! from upstream; each read names its upstream method in its doc comment.

use rusqlite::{Connection, params_from_iter};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ExecutionNode, OrchestrationV2PlanArtifact, OrchestrationV2ResponseCapability,
    OrchestrationV2RuntimeRequest, OrchestrationV2TurnItem,
};
use turin_contracts::{ProviderTurnId, ThreadId};

use crate::projection::error::ReadFailure;
use crate::projection::sql::{query_payload, query_payloads};
use crate::projection::store::read_thread;

/// `getPendingNativeUserInputs`: a provider turn's pending user-input requests
/// (native ones only; message-answerable requests are excluded), with the nodes
/// that own them and the request turn items.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingNativeUserInputs {
    pub runtime_requests: Vec<OrchestrationV2RuntimeRequest>,
    pub nodes: Vec<OrchestrationV2ExecutionNode>,
    pub turn_items: Vec<OrchestrationV2TurnItem>,
}

pub(crate) fn pending_native_user_inputs(
    conn: &Connection,
    thread_id: &ThreadId,
    provider_turn_id: &ProviderTurnId,
) -> Result<PendingNativeUserInputs, ReadFailure> {
    let requests: Vec<OrchestrationV2RuntimeRequest> = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_runtime_requests
         WHERE thread_id = ?1 AND provider_turn_id = ?2
           AND kind = 'user_input' AND status = 'pending'",
        rusqlite::params![thread_id.as_str(), provider_turn_id.as_str()],
    )?;
    let runtime_requests: Vec<OrchestrationV2RuntimeRequest> = requests
        .into_iter()
        .filter(|request| {
            !matches!(
                request.response_capability,
                OrchestrationV2ResponseCapability::Message
            )
        })
        .collect();
    if runtime_requests.is_empty() {
        return Ok(PendingNativeUserInputs::default());
    }

    let node_ids: Vec<String> = runtime_requests
        .iter()
        .map(|request| request.node_id.as_str().to_owned())
        .collect();
    let node_sql = format!(
        "SELECT payload_json FROM orchestration_v2_projection_nodes
         WHERE thread_id = ?1 AND node_id IN ({})",
        placeholders_from(2, node_ids.len())
    );
    let mut node_params = vec![thread_id.as_str().to_owned()];
    node_params.extend(node_ids);
    let nodes = query_payloads(conn, &node_sql, params_from_iter(node_params.iter()))?;

    let items: Vec<OrchestrationV2TurnItem> = query_payloads(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_turn_items
         WHERE thread_id = ?1 AND provider_turn_id = ?2
           AND type = 'user_input_request'",
        rusqlite::params![thread_id.as_str(), provider_turn_id.as_str()],
    )?;
    let request_ids: std::collections::HashSet<&str> = runtime_requests
        .iter()
        .map(|request| request.id.as_str())
        .collect();
    let turn_items = items
        .into_iter()
        .filter(|item| match item {
            OrchestrationV2TurnItem::UserInputRequest { request_id, .. } => {
                request_ids.contains(request_id.as_str())
            }
            _ => false,
        })
        .collect();

    Ok(PendingNativeUserInputs {
        runtime_requests,
        nodes,
        turn_items,
    })
}

/// `getPlan`: one plan artifact, `None` when the thread has no such plan.
/// Errors if the thread does not exist.
pub(crate) fn plan(
    conn: &Connection,
    thread_id: &ThreadId,
    plan_id: &str,
) -> Result<Option<OrchestrationV2PlanArtifact>, ReadFailure> {
    read_thread(conn, thread_id)?;
    Ok(query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_plans
         WHERE thread_id = ?1 AND plan_id = ?2",
        rusqlite::params![thread_id.as_str(), plan_id],
    )?)
}

/// `getRuntimeRequest`: one runtime request, `None` when absent. Errors if the
/// thread does not exist.
pub(crate) fn runtime_request(
    conn: &Connection,
    thread_id: &ThreadId,
    request_id: &str,
) -> Result<Option<OrchestrationV2RuntimeRequest>, ReadFailure> {
    read_thread(conn, thread_id)?;
    Ok(query_payload(
        conn,
        "SELECT payload_json FROM orchestration_v2_projection_runtime_requests
         WHERE thread_id = ?1 AND runtime_request_id = ?2",
        rusqlite::params![thread_id.as_str(), request_id],
    )?)
}

/// `?1` placeholders for `count` values starting at parameter `first`.
fn placeholders_from(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

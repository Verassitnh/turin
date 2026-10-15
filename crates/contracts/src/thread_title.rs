//! Port of upstream/packages/contracts/src/threadTitle.ts

use crate::base_schemas::{CommandId, IsoDateTime};
use serde::{Deserialize, Serialize};

/// An in-flight title regeneration, as MCP thread metadata reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTitleRegeneration {
    pub request_id: CommandId,
    pub started_at: IsoDateTime,
}

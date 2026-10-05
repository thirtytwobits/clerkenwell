//! The stored envelope format: a checksummed checkpoint, a window of
//! retained operations, and the principal each peer's operations are made by.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clerkenwell_events::Principal;
use serde::{Deserialize, Serialize};

use crate::{checkpoint_etag, corrupt_state, sha256_hex, CollaborationDocumentId, StoreResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationOperation {
    pub operation_id: String,
    pub sequence: u64,
    pub schema_version: u32,
    /// The principal whose commit accepted the operation.
    pub actor: Principal,
    /// Why the principal made it, when the commit said.
    pub intent: Option<String>,
    pub update_base64: String,
    pub update_sha256: String,
    pub update_bytes: usize,
}

impl CollaborationOperation {
    pub fn from_update(
        operation_id: String,
        sequence: u64,
        schema_version: u32,
        actor: Principal,
        intent: Option<String>,
        update: &[u8],
    ) -> Self {
        let update_sha256 = sha256_hex(update);
        Self {
            operation_id,
            sequence,
            schema_version,
            actor,
            intent,
            update_base64: BASE64.encode(update),
            update_sha256,
            update_bytes: update.len(),
        }
    }

    pub fn decode(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        let update = BASE64
            .decode(self.update_base64.as_bytes())
            .map_err(|error| {
                corrupt_state(
                    document,
                    format!(
                        "operation {} is not valid base64: {error}",
                        self.operation_id
                    ),
                )
            })?;
        if update.len() != self.update_bytes || sha256_hex(&update) != self.update_sha256 {
            return Err(corrupt_state(
                document,
                format!("operation {} failed checksum validation", self.operation_id),
            ));
        }
        Ok(update)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableCollaborationEnvelope {
    pub envelope_version: u32,
    pub entity: String,
    pub resource_id: String,
    pub schema_version: u32,
    pub generation: u64,
    pub checkpoint_sequence: u64,
    pub compacted_through_sequence: u64,
    pub checkpoint_update_base64: String,
    pub checkpoint_sha256: String,
    pub checkpoint_bytes: usize,
    pub retained_operations: Vec<CollaborationOperation>,
    /// The principal every operation under each peer is made by. Every peer
    /// the checkpoint holds operations of is bound.
    pub peers: BTreeMap<u64, Principal>,
}

impl DurableCollaborationEnvelope {
    pub fn checkpoint_update(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        let update = BASE64
            .decode(self.checkpoint_update_base64.as_bytes())
            .map_err(|error| {
                corrupt_state(document, format!("checkpoint is not valid base64: {error}"))
            })?;
        if update.len() != self.checkpoint_bytes || sha256_hex(&update) != self.checkpoint_sha256 {
            return Err(corrupt_state(
                document,
                "checkpoint failed checksum validation",
            ));
        }
        Ok(update)
    }

    /// The etag of the accepted state this envelope holds, as imports and
    /// authoring states report it.
    pub fn etag(&self) -> String {
        checkpoint_etag(&self.checkpoint_sha256)
    }
}

//! A small plan the integration tests share.

// Each test binary uses a different part of this module.
#![allow(dead_code)]

use clerkenwell_doc::CollaborationReplica;
use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec,
};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationAuditQuery, CollaborationAuditRecord, CollaborationAuthoringState,
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationService, CollaborationStoragePort, CommitPolicy, ImportFence, PeerBlock, PeerKey,
    StoreResult, StoredEnvelope,
};
use serde_json::Value;
use std::num::NonZeroU32;
use std::time::Duration;

/// The key the tests' store sets derive peer blocks from.
pub fn key() -> PeerKey {
    PeerKey::new([7; 32]).expect("a key of 32 bytes")
}

/// The principal the tests write as.
pub fn writer() -> Principal {
    Principal::new("writer", ActorKind::Human)
}

/// The principal the tests recover documents as.
pub fn operator() -> Principal {
    Principal::new("operator", ActorKind::Human)
}

/// A replica of what `state` delivers, writing under the first peer of a
/// block `service` allocates to `actor` for `document`, and that block.
pub fn client(
    service: &CollaborationService,
    document: &CollaborationDocumentId,
    actor: &Principal,
    state: &CollaborationAuthoringState,
) -> (CollaborationReplica, PeerBlock) {
    let replica = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("client replica");
    let block = service.allocate_peers(actor, document);
    replica.set_peer(block.base).expect("allocated peer");
    (replica, block)
}

/// An import of `update`, made from `base` by `actor` under `block`.
pub fn request(
    document: &CollaborationDocumentId,
    actor: &Principal,
    block: &PeerBlock,
    operation_id: &str,
    base: &str,
    update: String,
) -> CollaborationImportRequest {
    CollaborationImportRequest {
        document: document.clone(),
        schema_version: NOTE_PLAN.schema_version,
        operation_id: operation_id.to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: base.to_string(),
        update_base64: update,
        fence: ImportFence::Frontier,
        actor: actor.clone(),
        peer_nonces: vec![block.nonce.clone()],
        intent: None,
    }
}

pub const fn field(
    path: &'static str,
    storage_kind: GeneratedCollaborationStorageKind,
    container: Option<&'static str>,
    key: Option<&'static str>,
    conflict: GeneratedCollaborationConflict,
) -> GeneratedCollaborationFieldSpec {
    GeneratedCollaborationFieldSpec {
        path,
        storage_kind,
        container,
        container_template: None,
        key,
        identity_path: None,
        identity_variable: None,
        order_container: None,
        item_container_template: None,
        metadata_container: None,
        metadata_container_template: None,
        metadata_key: None,
        codec: GeneratedCollaborationValueCodec::String,
        value_schema: None,
        required: true,
        required_in_parent: true,
        conflict,
        writers: &[],
    }
}

static NOTE_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    GeneratedCollaborationFieldSpec {
        ..field(
            "note_id",
            GeneratedCollaborationStorageKind::Scalar,
            Some("note"),
            Some("note_id"),
            GeneratedCollaborationConflict::Immutable,
        )
    },
    field(
        "body",
        GeneratedCollaborationStorageKind::Text,
        Some("body"),
        None,
        GeneratedCollaborationConflict::Merge,
    ),
    field(
        "etag",
        GeneratedCollaborationStorageKind::DerivedRevision,
        None,
        None,
        GeneratedCollaborationConflict::Immutable,
    ),
];
pub static NOTE_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Note",
    id_field: "note_id",
    schema_version: 1,
    authoring_projection: "notes.authoringState",
    authoring_document: None,
    authoring_store_params: &[],
    import_mutation: "note.importUpdate",
    root_container: "note",
    fields: NOTE_FIELDS,
};
/// A validator that accepts every document.
pub fn accept(_: &Value) -> StoreResult<()> {
    Ok(())
}

pub static PLANS: &[GeneratedCollaborationEntitySpec] = &[NOTE_PLAN];

/// The commit policy these tests run under.
pub const POLICY: CommitPolicy = CommitPolicy {
    retained_operations: 8,
    attempts: match NonZeroU32::new(8) {
        Some(attempts) => attempts,
        None => panic!("attempts are non-zero"),
    },
    backoff: Duration::from_millis(2),
};

/// A port keeping its bytes in memory under sources and versions named
/// otherwise than [`MemoryStorage`] names them.
#[derive(Debug, Clone, Default)]
pub struct Renamed(pub MemoryStorage);

impl Renamed {
    fn inner_version(version: &str) -> &str {
        version.strip_prefix("renamed-").unwrap_or(version)
    }

    fn outer_version(version: String) -> String {
        format!("renamed-{version}")
    }
}

impl CollaborationStoragePort for Renamed {
    fn source(&self, document: &CollaborationDocumentId) -> String {
        format!("renamed:{}", self.0.source(document))
    }

    fn sources(&self, entity: &str) -> StoreResult<Vec<String>> {
        Ok(self
            .0
            .sources(entity)?
            .into_iter()
            .map(|source| format!("renamed:{source}"))
            .collect())
    }

    fn read(&self, source: &str) -> StoreResult<Option<StoredEnvelope>> {
        let inner = source.strip_prefix("renamed:").unwrap_or(source);
        Ok(self.0.read(inner)?.map(|stored| StoredEnvelope {
            version: Self::outer_version(stored.version),
            bytes: stored.bytes,
        }))
    }

    fn stamp(&self, source: &str) -> StoreResult<Option<String>> {
        let inner = source.strip_prefix("renamed:").unwrap_or(source);
        Ok(self.0.stamp(inner)?.map(Self::outer_version))
    }

    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>> {
        Ok(self
            .0
            .compare_and_swap(document, expected.map(Self::inner_version), bytes)?
            .map(Self::outer_version))
    }

    fn compare_and_remove(
        &self,
        document: &CollaborationDocumentId,
        expected: &str,
    ) -> StoreResult<bool> {
        self.0
            .compare_and_remove(document, Self::inner_version(expected))
    }

    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<String> {
        Ok(format!(
            "renamed:{}",
            self.0.preserve(document, label, bytes)?
        ))
    }

    fn append_audit(&self, record: &CollaborationAuditRecord) -> StoreResult<()> {
        self.0.append_audit(record)
    }

    fn audit(&self, query: &CollaborationAuditQuery) -> StoreResult<Vec<CollaborationAuditRecord>> {
        self.0.audit(query)
    }

    fn discard_audit_before(&self, unix_ms: u128) -> StoreResult<usize> {
        self.0.discard_audit_before(unix_ms)
    }
}

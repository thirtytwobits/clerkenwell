//! A small plan the integration tests share.

// Each test binary uses a different part of this module.
#![allow(dead_code)]

use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec,
};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationRecoveryAuditRecord, CollaborationStoragePort,
    CommitPolicy, StoreResult, StoredEnvelope,
};
use serde_json::Value;
use std::num::NonZeroU32;
use std::time::Duration;

const fn field(
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

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()> {
        self.0.append_recovery_audit(record)
    }

    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        self.0.recovery_audit()
    }
}

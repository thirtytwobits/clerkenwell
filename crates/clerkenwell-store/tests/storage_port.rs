//! The collaboration service commits through a storage port implemented
//! outside this crate.

use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec,
};
use clerkenwell_store::StoreResult;
use clerkenwell_store::{
    CollaborationCommit, CollaborationCommitOutcome, CollaborationDocumentId,
    CollaborationDocumentInspection, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationPublicationScan, CollaborationRecoveryAction, CollaborationRecoveryAuditRecord,
    CollaborationService, CollaborationStoragePort, DurableCollaborationEnvelope,
    LocalFileCollaborationStorage,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A port that counts the commits it is asked for and keeps its envelopes in
/// a local-file store it owns.
#[derive(Debug, Clone)]
struct CountingPort {
    inner: LocalFileCollaborationStorage,
    commits: Arc<AtomicUsize>,
}

impl CollaborationStoragePort for CountingPort {
    fn load(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<DurableCollaborationEnvelope>> {
        self.inner.load(document)
    }

    fn compare_and_swap(
        &self,
        commit: CollaborationCommit,
    ) -> StoreResult<CollaborationCommitOutcome> {
        self.commits.fetch_add(1, Ordering::SeqCst);
        self.inner.compare_and_swap(commit)
    }

    fn acknowledge_publication(
        &self,
        document: &CollaborationDocumentId,
        generation: u64,
    ) -> StoreResult<()> {
        self.inner.acknowledge_publication(document, generation)
    }

    fn delete(&self, document: &CollaborationDocumentId) -> StoreResult<()> {
        self.inner.delete(document)
    }

    fn move_document(
        &self,
        source: &CollaborationDocumentId,
        destination: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>> {
        self.inner.move_document(source, destination, relative_path)
    }

    fn update_relative_path(
        &self,
        document: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>> {
        self.inner.update_relative_path(document, relative_path)
    }

    fn list_publications(&self, entity: &str) -> StoreResult<CollaborationPublicationScan> {
        self.inner.list_publications(entity)
    }

    fn list_entity(&self, entity: &str) -> StoreResult<Vec<DurableCollaborationEnvelope>> {
        self.inner.list_entity(entity)
    }

    fn export_for_recovery(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        self.inner.export_for_recovery(document)
    }

    fn quarantine(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf> {
        self.inner.quarantine(document, reason)
    }

    fn inspect(
        &self,
        entity: Option<&str>,
        resource_id: Option<&str>,
        limit: Option<usize>,
    ) -> StoreResult<(Vec<CollaborationDocumentInspection>, bool)> {
        CollaborationStoragePort::inspect(&self.inner, entity, resource_id, limit)
    }

    fn verify(&self, document: &CollaborationDocumentId) -> CollaborationDocumentInspection {
        CollaborationStoragePort::verify(&self.inner, document)
    }

    fn repair(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf> {
        CollaborationStoragePort::repair(&self.inner, document, reason)
    }

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()> {
        self.inner.append_recovery_audit(record)
    }

    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        self.inner.recovery_audit()
    }
}

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
        conflict,
    }
}

static NOTE_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    field(
        "note_id",
        GeneratedCollaborationStorageKind::Scalar,
        Some("note"),
        Some("note_id"),
        GeneratedCollaborationConflict::Immutable,
    ),
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
static NOTE_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Note",
    id_field: "note_id",
    substrate: "loro",
    schema_version: 1,
    migration_ids: &[],
    authoring_projection: "notes.authoringState",
    import_mutation: "note.importLoroUpdate",
    root_container: "note",
    fields: NOTE_FIELDS,
};
static PLANS: &[GeneratedCollaborationEntitySpec] = &[NOTE_PLAN];

#[test]
fn a_storage_port_implemented_outside_the_crate_carries_every_commit() {
    let root = tempfile::tempdir().expect("temp store");
    let commits = Arc::new(AtomicUsize::new(0));
    let service = CollaborationService::with_storage(
        CountingPort {
            inner: LocalFileCollaborationStorage::new(root.path(), PLANS),
            commits: commits.clone(),
        },
        PLANS,
    );
    let seed = json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" });
    let document = CollaborationDocumentId::new("Note", "note-1");
    let relative_path = "notes/note-1.yaml";

    service
        .bootstrap(&NOTE_PLAN, &document, relative_path, &seed, |_| Ok(()))
        .expect("bootstrap through the port");
    let state = service
        .authoring_state(&NOTE_PLAN, &document)
        .expect("read through the port");

    let mut edited = seed.clone();
    edited["body"] = Value::from("Written through a foreign port.");
    let mut client = LoroAuthoringDocument::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("hydrate client");
    client.replace_document(&edited).expect("edit client");
    let imported = service
        .import(
            &NOTE_PLAN,
            CollaborationImportRequest {
                document: document.clone(),
                relative_path: relative_path.to_string(),
                schema_version: state.schema_version,
                operation_id: "foreign-port-edit".to_string(),
                exchange_mode: CollaborationExchangeMode::Incremental,
                base_frontier_base64: state.accepted_frontier_base64.clone(),
                update_base64: client
                    .export_incremental_update_base64(&state.accepted_frontier_base64)
                    .expect("incremental update"),
            },
            |_| Ok(()),
        )
        .expect("import through the port");
    service
        .acknowledge_publication(&document, imported.generation)
        .expect("acknowledge through the port");
    service
        .quarantine(&document, "foreign port evidence")
        .expect("quarantine through the port");

    assert_eq!(commits.load(Ordering::SeqCst), 2, "seed and edit");
    let accepted = service
        .detail(&NOTE_PLAN, &document)
        .expect("materialise through the port")
        .expect("accepted document");
    assert_eq!(accepted["body"], edited["body"]);
    assert!(
        !service
            .storage()
            .load(&document)
            .expect("reload")
            .expect("envelope")
            .publication_pending
    );
    let audit = service.recovery_audit().expect("audit through the port");
    assert_eq!(
        audit.iter().map(|record| record.action).collect::<Vec<_>>(),
        vec![CollaborationRecoveryAction::Quarantine]
    );
}

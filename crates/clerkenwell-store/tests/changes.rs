//! A service announces each change to a document's accepted state.

mod support;

use std::sync::{Arc, Mutex};

use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_events::{ChangeEvent, ChangeKind};
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationService, ImportFence, StoreError, StoreResult,
};
use serde_json::{json, Value};
use support::{accept, NOTE_PLAN, PLANS, POLICY};

const RELATIVE_PATH: &str = "notes/note-1.yaml";

fn note(resource_id: &str) -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", resource_id)
}

fn seed() -> Value {
    json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" })
}

/// A service over a fresh store, and everything it announces.
fn heard_service() -> (
    tempfile::TempDir,
    CollaborationService,
    Arc<Mutex<Vec<ChangeEvent>>>,
) {
    let root = tempfile::tempdir().expect("temp store");
    let service = CollaborationService::new(root.path(), PLANS, POLICY);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    service
        .changes()
        .listen(move |change| log.lock().unwrap().push(change.clone()));
    (root, service, heard)
}

fn bootstrap(service: &CollaborationService) {
    service
        .bootstrap(&NOTE_PLAN, &note("note-1"), RELATIVE_PATH, &seed(), accept)
        .expect("bootstrap");
}

/// Imports an edit that sets the note's body, from what the store holds now.
fn edit(
    service: &CollaborationService,
    operation_id: &str,
    body: &str,
) -> clerkenwell_store::CollaborationImportResult {
    let state = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");
    let mut client = LoroAuthoringDocument::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("client");
    let mut edited = client.materialized_document("edit").expect("document");
    edited["body"] = Value::from(body);
    client.replace_document(&edited).expect("edit");
    service
        .import(
            &NOTE_PLAN,
            CollaborationImportRequest {
                document: note("note-1"),
                relative_path: RELATIVE_PATH.to_string(),
                schema_version: NOTE_PLAN.schema_version,
                operation_id: operation_id.to_string(),
                exchange_mode: CollaborationExchangeMode::Incremental,
                base_frontier_base64: state.accepted_frontier_base64.clone(),
                update_base64: client
                    .export_incremental_update_base64(&state.accepted_frontier_base64)
                    .expect("update"),
                fence: ImportFence::Frontier,
            },
            accept,
        )
        .expect("import")
}

#[test]
fn each_accepted_commit_announces_the_state_it_took_the_document_from_and_to() {
    let (_root, service, heard) = heard_service();
    bootstrap(&service);
    let results: Vec<_> = ["first", "second"]
        .iter()
        .map(|body| edit(&service, &format!("edit-{body}"), body))
        .collect();

    let heard = heard.lock().unwrap();
    assert_eq!(heard.len(), 1 + results.len());
    assert!(heard
        .iter()
        .all(|change| change.kind == ChangeKind::Committed));
    assert!(heard.iter().all(|change| change.subject == "Note/note-1"));
    let created = &heard[0];
    assert_eq!(created.data.etag_before, None);
    assert!(created.data.frontier_after.is_some());
    for (previous, change) in heard.iter().zip(heard.iter().skip(1)) {
        assert_eq!(change.data.etag_before, previous.data.etag_after);
        assert_eq!(change.data.frontier_before, previous.data.frontier_after);
        assert_eq!(
            change.generation,
            previous.generation.map(|generation| generation + 1)
        );
    }
    for (change, result) in heard.iter().skip(1).zip(&results) {
        assert_eq!(change.generation, Some(result.generation));
        assert_eq!(
            change.data.etag_after.as_deref(),
            Some(result.etag.as_str())
        );
        assert_eq!(
            change.data.frontier_after.as_deref(),
            Some(result.accepted_frontier_base64.as_str())
        );
        assert_eq!(
            change.data.operation_id.as_deref(),
            Some(result.operation_id.as_str())
        );
    }
}

#[test]
fn what_does_not_change_the_accepted_state_announces_nothing() {
    let (_root, service, heard) = heard_service();
    bootstrap(&service);
    let accepted = edit(&service, "edit-once", "Once.");
    let announced = heard.lock().unwrap().len();

    bootstrap(&service);
    let state = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");
    let refused = service.import(
        &NOTE_PLAN,
        CollaborationImportRequest {
            document: note("note-1"),
            relative_path: RELATIVE_PATH.to_string(),
            schema_version: NOTE_PLAN.schema_version,
            operation_id: "edit-refused".to_string(),
            exchange_mode: CollaborationExchangeMode::Incremental,
            base_frontier_base64: state.accepted_frontier_base64.clone(),
            update_base64: {
                let mut client = LoroAuthoringDocument::from_versioned_update_base64(
                    &NOTE_PLAN,
                    state.schema_version,
                    &state.update_base64,
                )
                .expect("client");
                let mut edited = client.materialized_document("edit").expect("document");
                edited["body"] = Value::from("Refused.");
                client.replace_document(&edited).expect("edit");
                client
                    .export_incremental_update_base64(&state.accepted_frontier_base64)
                    .expect("update")
            },
            fence: ImportFence::Frontier,
        },
        |_: &Value| -> StoreResult<()> { Err(StoreError::invalid_request("Refused.")) },
    );
    assert!(refused.is_err());
    service
        .acknowledge_publication(&note("note-1"), accepted.generation)
        .expect("acknowledge");
    service
        .delete(&note("note-absent"))
        .expect("delete nothing");

    assert_eq!(heard.lock().unwrap().len(), announced);
}

#[test]
fn a_move_announces_where_the_document_went_and_where_it_came_from() {
    let (_root, service, heard) = heard_service();
    bootstrap(&service);
    let generation = service
        .move_document(&note("note-1"), &note("note-2"), "notes/note-2.yaml")
        .expect("move")
        .expect("moved");

    let heard = heard.lock().unwrap();
    let moved = heard.last().expect("an announcement");
    assert_eq!(moved.kind, ChangeKind::Moved);
    assert_eq!(moved.subject, "Note/note-2");
    assert_eq!(moved.data.moved_from.as_deref(), Some("note-1"));
    assert_eq!(moved.generation, Some(generation));
    assert_eq!(moved.data.etag_before, moved.data.etag_after);
    assert_eq!(moved.data.etag_after, heard[0].data.etag_after);
    assert_eq!(moved.data.frontier_after, heard[0].data.frontier_after);
}

#[test]
fn a_deletion_announces_the_state_it_removed() {
    let (_root, service, heard) = heard_service();
    bootstrap(&service);
    service.delete(&note("note-1")).expect("delete");

    let heard = heard.lock().unwrap();
    let [created, deleted] = heard.as_slice() else {
        panic!("a creation and a deletion, not {heard:?}");
    };
    assert_eq!(deleted.kind, ChangeKind::Deleted);
    assert_eq!(deleted.subject, created.subject);
    assert_eq!(deleted.generation, created.generation);
    assert_eq!(deleted.data.etag_before, created.data.etag_after);
    assert_eq!(deleted.data.etag_after, None);
}

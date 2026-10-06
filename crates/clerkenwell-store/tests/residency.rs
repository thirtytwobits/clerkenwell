//! A service holds the documents it reads, and serves them again only while
//! the storage port vouches that their bytes are unchanged.

mod support;

use clerkenwell_store::testing::{self, MemoryStorage};
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationService, CollaborationStores, StoreError, StoreResult,
};
use serde_json::{json, Value};
use support::{accept, client, key, operator, request, writer, NOTE_PLAN, PLANS, POLICY};

fn note(resource_id: &str) -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", resource_id)
}

fn service(port: &MemoryStorage) -> CollaborationService {
    CollaborationService::new(port.clone(), PLANS, POLICY, key(), "notes")
}

fn bootstrap(service: &CollaborationService, resource_id: &str) {
    service
        .bootstrap(
            &NOTE_PLAN,
            &note(resource_id),
            &json!({ "note_id": resource_id, "body": "Seeded.", "etag": "" }),
            accept,
        )
        .expect("bootstrap");
}

/// Imports an edit setting the note's body, from what `service` reads now,
/// judged by `validate`.
fn edit(
    service: &CollaborationService,
    body: &str,
    validate: impl Fn(&Value) -> StoreResult<()>,
) -> StoreResult<String> {
    let state = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");
    let (mut client, block) = client(service, &note("note-1"), &writer(), &state);
    let mut edited = client.materialized_document("edit").expect("document");
    edited["body"] = Value::from(body);
    client.replace_document(&edited).expect("edit");
    service
        .import(
            &NOTE_PLAN,
            request(
                &note("note-1"),
                &writer(),
                &block,
                &format!("edit-{body}"),
                &state.accepted_frontier_base64,
                client
                    .export_incremental_update_base64(&state.accepted_frontier_base64)
                    .expect("update"),
            ),
            validate,
        )
        .map(|imported| imported.etag)
}

fn body(service: &CollaborationService) -> Value {
    service
        .detail(&NOTE_PLAN, &note("note-1"))
        .expect("detail")
        .expect("a note")["body"]
        .clone()
}

#[test]
fn a_document_read_again_is_served_without_reading_its_bytes() {
    let port = MemoryStorage::default();
    let service = service(&port);
    bootstrap(&service, "note-1");
    let first = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");
    let reads = port.reads();

    let again = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");
    body(&service);

    assert_eq!(port.reads(), reads);
    assert_eq!(again, first);
}

#[test]
fn a_commit_is_served_to_the_next_read_as_it_was_accepted() {
    let port = MemoryStorage::default();
    let service = service(&port);
    bootstrap(&service, "note-1");
    let before = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");

    let etag = edit(&service, "Edited.", accept).expect("import");

    let after = service
        .authoring_state(
            &NOTE_PLAN,
            &note("note-1"),
            Some(&before.accepted_frontier_base64),
        )
        .expect("authoring state");
    assert_eq!(after.etag, etag);
    assert_ne!(
        after.accepted_frontier_base64,
        before.accepted_frontier_base64
    );
    assert_eq!(body(&service), "Edited.");
}

#[test]
fn a_write_that_bypasses_the_service_is_seen_by_its_next_read() {
    for port in [MemoryStorage::default(), MemoryStorage::without_stamps()] {
        let reader = service(&port);
        let writer = service(&port);
        bootstrap(&writer, "note-1");
        assert_eq!(body(&reader), "Seeded.");

        let etag = edit(&writer, "Written elsewhere.", accept).expect("import");

        assert_eq!(body(&reader), "Written elsewhere.");
        assert_eq!(
            reader
                .authoring_state(&NOTE_PLAN, &note("note-1"), None)
                .expect("authoring state")
                .etag,
            etag
        );
    }
}

#[test]
fn without_a_stamp_the_service_reads_the_bytes_and_keeps_what_they_match() {
    let port = MemoryStorage::without_stamps();
    let service = service(&port);
    bootstrap(&service, "note-1");
    let first = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");
    let reads = port.reads();

    let again = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");

    assert!(port.reads() > reads);
    assert_eq!(again, first);
}

#[test]
fn a_refused_import_leaves_what_readers_are_served_unchanged() {
    let port = MemoryStorage::default();
    let service = service(&port);
    bootstrap(&service, "note-1");
    let before = service
        .authoring_state(&NOTE_PLAN, &note("note-1"), None)
        .expect("authoring state");

    let refused = edit(&service, "Refused.", |_: &Value| {
        Err(StoreError::invalid_request("Refused."))
    });

    assert!(refused.is_err());
    assert_eq!(
        service
            .authoring_state(&NOTE_PLAN, &note("note-1"), None)
            .expect("authoring state"),
        before
    );
    assert_eq!(body(&service), "Seeded.");
}

#[test]
fn a_deleted_document_is_no_longer_served() {
    let port = MemoryStorage::default();
    let service = service(&port);
    bootstrap(&service, "note-1");
    body(&service);

    service
        .delete(&operator(), &note("note-1"))
        .expect("delete");

    assert!(service
        .detail(&NOTE_PLAN, &note("note-1"))
        .expect("detail")
        .is_none());
}

#[test]
fn the_counters_report_what_the_stores_hold_until_they_let_it_go() {
    let stores = CollaborationStores::new(PLANS, POLICY, key());
    let store = stores
        .register("notes", MemoryStorage::default())
        .expect("register");
    for resource_id in ["note-1", "note-2"] {
        bootstrap(&store, resource_id);
    }
    let held = store.summaries("Note").expect("summaries");
    let counters = stores.counters();
    assert_eq!(counters.resident_documents, held.len() as u64);
    assert_eq!(
        counters.resident_checkpoint_bytes,
        held.iter()
            .map(|summary| summary.checkpoint_bytes as u64)
            .sum::<u64>()
    );

    store.delete(&operator(), &note("note-1")).expect("delete");
    assert_eq!(stores.counters().resident_documents, held.len() as u64 - 1);

    stores.forget("notes");
    drop(store);
    let counters = stores.counters();
    assert_eq!(counters.resident_documents, 0);
    assert_eq!(counters.resident_checkpoint_bytes, 0);
}

#[test]
fn a_summary_describes_a_stored_document_as_its_envelope_does() {
    let port = MemoryStorage::default();
    let service = service(&port);
    bootstrap(&service, "note-1");
    edit(&service, "Edited once.", accept).expect("import");
    edit(&service, "Edited twice.", accept).expect("import");

    let summary = service
        .summary(&note("note-1"))
        .expect("summary")
        .expect("a summary");
    let envelope = testing::load(&service, &note("note-1"))
        .expect("load")
        .expect("an envelope");

    assert_eq!(summary.document, note("note-1"));
    assert_eq!(summary.generation, envelope.generation);
    assert_eq!(summary.etag, envelope.etag());
    assert_eq!(summary.retained_count, envelope.retained_operations.len());
    assert_eq!(
        summary.retained_sequences,
        envelope
            .retained_operations
            .first()
            .zip(envelope.retained_operations.last())
            .map(|(first, last)| (first.sequence, last.sequence))
    );
}

#[test]
fn summaries_list_every_stored_document_in_resource_order() {
    let port = MemoryStorage::default();
    let service = service(&port);
    for resource_id in ["note-3", "note-1", "note-2"] {
        bootstrap(&service, resource_id);
    }

    let listed: Vec<String> = service
        .summaries("Note")
        .expect("summaries")
        .into_iter()
        .map(|summary| summary.document.resource_id)
        .collect();

    assert_eq!(listed, ["note-1", "note-2", "note-3"]);
}

#[test]
fn a_read_document_and_its_summary_are_of_one_version() {
    let port = MemoryStorage::default();
    let service = service(&port);
    bootstrap(&service, "note-1");

    let etag = edit(&service, "Read together.", accept).expect("import");

    let read = service
        .read_document(&NOTE_PLAN, &note("note-1"))
        .expect("read")
        .expect("a document");
    assert_eq!(read.summary.etag, etag);
    assert_eq!(read.document["body"], "Read together.");
    assert_eq!(
        Some(read.document),
        service.detail(&NOTE_PLAN, &note("note-1")).expect("detail")
    );
}

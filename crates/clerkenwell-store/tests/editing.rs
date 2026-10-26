//! The store writes a principal's edit of a document as the operations
//! between the version read and the edit, under a peer allocated to the
//! principal: a whole document fenced on the etag it was read at, or a text
//! replacement fenced on the frontier it was read at.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clerkenwell_events::{ChangeEvent, ChangeKind, Principal};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentEdit, CollaborationDocumentId, CollaborationDocumentRead,
    CollaborationService, CollaborationTextEdit, StoreError, StoreErrorKind, StoreResult,
};
use serde_json::{json, Value};
use support::{accept, key, operator, writer, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

/// A service holding the seeded note, and everything it announces after the
/// seed.
fn seeded() -> (CollaborationService, Arc<Mutex<Vec<ChangeEvent>>>) {
    let service =
        CollaborationService::new(MemoryStorage::default(), PLANS, POLICY, key(), "notes");
    service
        .bootstrap(
            &NOTE_PLAN,
            &note(),
            &json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" }),
            accept,
        )
        .expect("bootstrap");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    service
        .changes()
        .listen(move |change| log.lock().unwrap().push(change.clone()));
    (service, heard)
}

fn read_note(service: &CollaborationService) -> CollaborationDocumentRead {
    service
        .read_document(&NOTE_PLAN, &note())
        .expect("read")
        .expect("the note")
}

/// `read`'s document with its body set to `body`, edited by `actor`.
fn body_edit(
    read: &CollaborationDocumentRead,
    actor: &Principal,
    operation_id: &str,
    body: &str,
) -> CollaborationDocumentEdit {
    let mut edited = read.document.clone();
    edited["body"] = Value::from(body);
    CollaborationDocumentEdit {
        document: note(),
        operation_id: operation_id.to_owned(),
        edited,
        etag: read.summary.etag.clone(),
        actor: actor.clone(),
        intent: None,
    }
}

/// A replacement of the body `read` holds, by `actor`.
fn text_edit(
    read: &CollaborationDocumentRead,
    actor: &Principal,
    operation_id: &str,
    replacement: &str,
) -> CollaborationTextEdit {
    CollaborationTextEdit {
        document: note(),
        operation_id: operation_id.to_owned(),
        field_path: "body".to_owned(),
        identities: HashMap::new(),
        frontier_base64: read.accepted_frontier_base64.clone(),
        read: read.document["body"].as_str().expect("a body").to_owned(),
        replacement: replacement.to_owned(),
        actor: actor.clone(),
        intent: None,
    }
}

fn refusal_code(error: &StoreError) -> Option<&str> {
    error.data.as_ref().and_then(|data| data["code"].as_str())
}

#[test]
fn an_edit_of_the_document_read_is_accepted_as_its_principal_and_announced() {
    let (service, heard) = seeded();
    let read = read_note(&service);
    let mut edit = body_edit(&read, &writer(), "edit-body", "Edited.");
    edit.intent = Some("tidy the note".to_owned());

    let edited = service
        .edit(&NOTE_PLAN, edit, accept)
        .expect("an edit of the document read");

    assert_eq!(edited.materialized["body"], "Edited.");
    let now = read_note(&service);
    assert_eq!(now.document["body"], "Edited.");
    assert_eq!(now.summary.etag, edited.etag);
    assert_eq!(now.summary.generation, read.summary.generation + 1);

    let attribution = service
        .attribution(&note())
        .expect("attribution")
        .expect("the note");
    let operation = attribution.operations.last().expect("the edit");
    assert_eq!(operation.operation_id, "edit-body");
    assert_eq!(operation.actor, writer());
    assert_eq!(operation.intent.as_deref(), Some("tidy the note"));
    let new_peers = attribution
        .peers
        .iter()
        .filter(|(_, principal)| **principal == writer())
        .count();
    assert_eq!(new_peers, 1, "the edit is written under one allocated peer");

    let heard = heard.lock().unwrap();
    let [announced] = heard.as_slice() else {
        panic!("one announcement, not {}", heard.len());
    };
    assert_eq!(announced.kind, ChangeKind::Committed);
    assert_eq!(announced.actorid, writer().id);
    assert_eq!(announced.intent.as_deref(), Some("tidy the note"));
    assert_eq!(announced.generation, Some(now.summary.generation));
}

#[test]
fn an_edit_from_a_stale_etag_is_refused_with_the_accepted_document_and_its_etag() {
    let (service, _) = seeded();
    let stale = read_note(&service);
    service
        .edit(
            &NOTE_PLAN,
            body_edit(&stale, &operator(), "first", "First."),
            accept,
        )
        .expect("the first edit");
    let accepted = read_note(&service);

    let refused = service
        .edit(
            &NOTE_PLAN,
            body_edit(&stale, &writer(), "second", "Second."),
            accept,
        )
        .expect_err("an edit of a document changed since it was read");

    assert_eq!(refused.kind, StoreErrorKind::Conflict);
    let data = refused.data.as_ref().expect("refusal data");
    assert_eq!(data["conflict_kind"], "collaboration_revision");
    assert_eq!(data["current"], accepted.document);
    assert_eq!(data["current_etag"], accepted.summary.etag);
    assert_eq!(
        read_note(&service),
        accepted,
        "the refused edit changed nothing"
    );
}

#[test]
fn an_edit_that_changes_nothing_commits_nothing() {
    let (service, heard) = seeded();
    let read = read_note(&service);
    let edit = body_edit(&read, &writer(), "unchanged", "Seeded.");

    let edited = service
        .edit(&NOTE_PLAN, edit, accept)
        .expect("an edit that changes nothing");

    assert_eq!(edited.etag, read.summary.etag);
    assert_eq!(read_note(&service), read);
    assert!(heard.lock().unwrap().is_empty(), "nothing is announced");
}

#[test]
fn an_edit_the_application_refuses_changes_nothing() {
    let (service, heard) = seeded();
    let read = read_note(&service);
    let refuse = |document: &Value| -> StoreResult<()> {
        if document["body"] == "Refused." {
            Err(StoreError::invalid_request("refused"))
        } else {
            Ok(())
        }
    };

    service
        .edit(
            &NOTE_PLAN,
            body_edit(&read, &writer(), "refused", "Refused."),
            refuse,
        )
        .expect_err("a document the validator refuses");

    assert_eq!(read_note(&service), read);
    assert!(heard.lock().unwrap().is_empty(), "nothing is announced");
}

#[test]
fn concurrent_text_edits_from_one_frontier_merge() {
    let (service, heard) = seeded();
    let read = read_note(&service);

    service
        .edit_text(
            &NOTE_PLAN,
            text_edit(&read, &writer(), "writer-text", "Written."),
            accept,
        )
        .expect("the writer's replacement");
    service
        .edit_text(
            &NOTE_PLAN,
            text_edit(&read, &operator(), "operator-text", "Operated."),
            accept,
        )
        .expect("a concurrent replacement from the same frontier");

    let body = read_note(&service).document["body"]
        .as_str()
        .expect("a body")
        .to_owned();
    assert!(body.contains("Written."), "{body}");
    assert!(body.contains("Operated."), "{body}");
    assert!(!body.contains("Seeded."), "{body}");
    let actors: Vec<String> = heard
        .lock()
        .unwrap()
        .iter()
        .map(|change| change.actorid.clone())
        .collect();
    assert_eq!(actors, [writer().id, operator().id]);
}

#[test]
fn a_text_edit_is_written_under_a_peer_allocated_to_its_principal() {
    let (service, _) = seeded();
    let read = read_note(&service);

    let edited = service
        .edit_text(
            &NOTE_PLAN,
            text_edit(&read, &writer(), "writer-text", "Written."),
            accept,
        )
        .expect("a replacement");

    assert_eq!(edited.materialized["body"], "Written.");
    let attribution = service
        .attribution(&note())
        .expect("attribution")
        .expect("the note");
    assert_eq!(
        attribution
            .operations
            .last()
            .map(|operation| &operation.actor),
        Some(&writer())
    );
    assert!(attribution
        .peers
        .values()
        .any(|principal| *principal == writer()));
}

#[test]
fn a_text_edit_of_text_other_than_what_was_read_is_refused() {
    let (service, heard) = seeded();
    let read = read_note(&service);
    let mut edit = text_edit(&read, &writer(), "misread", "Written.");
    edit.read = "Not what was read.".to_owned();

    let refused = service
        .edit_text(&NOTE_PLAN, edit, accept)
        .expect_err("text other than what the frontier holds");

    assert_eq!(refused.kind, StoreErrorKind::InvalidRequest);
    assert_eq!(refusal_code(&refused), Some("collaboration_text_mismatch"));
    assert_eq!(read_note(&service), read);
    assert!(heard.lock().unwrap().is_empty(), "nothing is announced");
}

#[test]
fn editing_a_document_that_is_not_stored_is_refused_as_not_found() {
    let (service, _) = seeded();
    let read = read_note(&service);
    let mut edit = body_edit(&read, &writer(), "absent", "Edited.");
    edit.document = CollaborationDocumentId::new("Note", "note-2");

    let refused = service
        .edit(&NOTE_PLAN, edit, accept)
        .expect_err("no such note");

    assert_eq!(refused.kind, StoreErrorKind::NotFound);
}

#[test]
fn a_retried_edit_is_answered_with_the_result_it_was_accepted_with() {
    let (service, heard) = seeded();
    let read = read_note(&service);
    let edit = body_edit(&read, &writer(), "edit-body", "Edited.");
    let accepted = service
        .edit(&NOTE_PLAN, edit.clone(), accept)
        .expect("the edit");

    let retried = service
        .edit(&NOTE_PLAN, edit, accept)
        .expect("the same edit again");

    assert!(retried.duplicate);
    assert_eq!(retried.etag, accepted.etag);
    assert_eq!(retried.generation, accepted.generation);
    assert_eq!(
        heard.lock().unwrap().len(),
        1,
        "the retry announces nothing"
    );
}

#[test]
fn a_retried_edit_is_a_duplicate_however_far_the_document_has_moved() {
    let (service, _) = seeded();
    let read = read_note(&service);
    let edit = body_edit(&read, &writer(), "edit-body", "Edited.");
    service
        .edit(&NOTE_PLAN, edit.clone(), accept)
        .expect("the edit");
    let after = read_note(&service);
    service
        .edit(
            &NOTE_PLAN,
            body_edit(&after, &operator(), "later", "Later."),
            accept,
        )
        .expect("a later edit");
    let moved = read_note(&service);

    let retried = service
        .edit(&NOTE_PLAN, edit, accept)
        .expect("the first edit again");

    assert!(retried.duplicate);
    assert_eq!(read_note(&service), moved, "the retry changed nothing");
}

#[test]
fn a_retried_text_edit_is_answered_with_the_result_it_was_accepted_with() {
    let (service, heard) = seeded();
    let read = read_note(&service);
    let edit = text_edit(&read, &writer(), "writer-text", "Written.");
    let accepted = service
        .edit_text(&NOTE_PLAN, edit.clone(), accept)
        .expect("the replacement");

    let retried = service
        .edit_text(&NOTE_PLAN, edit, accept)
        .expect("the same replacement again");

    assert!(retried.duplicate);
    assert_eq!(retried.etag, accepted.etag);
    assert_eq!(read_note(&service).document["body"], "Written.");
    assert_eq!(
        heard.lock().unwrap().len(),
        1,
        "the retry announces nothing"
    );
}

#[test]
fn an_edit_under_another_principals_operation_id_is_refused() {
    let (service, _) = seeded();
    let read = read_note(&service);
    service
        .edit(
            &NOTE_PLAN,
            body_edit(&read, &operator(), "shared-id", "Operated."),
            accept,
        )
        .expect("the operator's edit");
    let after = read_note(&service);

    let refused = service
        .edit(
            &NOTE_PLAN,
            body_edit(&after, &writer(), "shared-id", "Written."),
            accept,
        )
        .expect_err("another principal's operation id");

    assert_eq!(refused.kind, StoreErrorKind::InvalidRequest);
    assert_eq!(
        refusal_code(&refused),
        Some("collaboration_operation_id_collision")
    );
    assert_eq!(read_note(&service), after);
}

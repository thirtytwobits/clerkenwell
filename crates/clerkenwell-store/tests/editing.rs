//! The store writes a principal's edit of a document as the operations
//! between the version read and the edit, under a peer allocated to the
//! principal: a whole document fenced on the etag it was read at, or a text
//! replacement fenced on the frontier it was read at.

mod support;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use clerkenwell_events::{ChangeEvent, ChangeKind, Principal};
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationAuditEvent, CollaborationAuditQuery, CollaborationAuditRecord,
    CollaborationDocumentEdit, CollaborationDocumentId, CollaborationDocumentRead,
    CollaborationImportResult, CollaborationService, CollaborationStoragePort,
    CollaborationTextEdit, StoreError, StoreErrorKind, StoreResult, StoredEnvelope,
};
use serde_json::{json, Value};
use support::{accept, field, key, operator, writer, NOTE_PLAN, PLANS, POLICY};

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

/// A draft whose body a writer replaces only from its current text.
static DRAFT_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    field(
        "draft_id",
        GeneratedCollaborationStorageKind::Scalar,
        Some("draft"),
        Some("draft_id"),
        GeneratedCollaborationConflict::Immutable,
    ),
    field(
        "body",
        GeneratedCollaborationStorageKind::Text,
        Some("body"),
        None,
        GeneratedCollaborationConflict::Explicit,
    ),
];
static DRAFT_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Draft",
    id_field: "draft_id",
    schema_version: 1,
    authoring_projection: "drafts.authoringState",
    authoring_document: None,
    authoring_store_params: &[],
    import_mutation: "draft.importUpdate",
    root_container: "draft",
    fields: DRAFT_FIELDS,
};
static DRAFT_PLANS: &[GeneratedCollaborationEntitySpec] = &[DRAFT_PLAN];

#[test]
fn a_retried_text_edit_is_a_duplicate_after_a_later_edit_of_an_explicit_field() {
    let draft = CollaborationDocumentId::new("Draft", "draft-1");
    let service = CollaborationService::new(
        MemoryStorage::default(),
        DRAFT_PLANS,
        POLICY,
        key(),
        "drafts",
    );
    service
        .bootstrap(
            &DRAFT_PLAN,
            &draft,
            &json!({ "draft_id": "draft-1", "body": "First." }),
            accept,
        )
        .expect("bootstrap");
    let read = |service: &CollaborationService| {
        service
            .read_document(&DRAFT_PLAN, &draft)
            .expect("read")
            .expect("the draft")
    };
    let replace = |read: &CollaborationDocumentRead, operation_id: &str, replacement: &str| {
        CollaborationTextEdit {
            document: draft.clone(),
            operation_id: operation_id.to_owned(),
            field_path: "body".to_owned(),
            identities: HashMap::new(),
            frontier_base64: read.accepted_frontier_base64.clone(),
            read: read.document["body"].as_str().expect("a body").to_owned(),
            replacement: replacement.to_owned(),
            actor: writer(),
            intent: None,
        }
    };
    let first = replace(&read(&service), "second", "Second.");
    let accepted = service
        .edit_text(&DRAFT_PLAN, first.clone(), accept)
        .expect("the first replacement");
    service
        .edit_text(
            &DRAFT_PLAN,
            replace(&read(&service), "third", "Third."),
            accept,
        )
        .expect("a replacement from the text the first left");
    let moved = read(&service);

    let retried = service
        .edit_text(&DRAFT_PLAN, first, accept)
        .expect("the first replacement again");

    assert!(retried.duplicate);
    assert_eq!(retried.operation_id, accepted.operation_id);
    assert_eq!(read(&service), moved, "the retry changed nothing");
}

/// Work a test runs in the middle of a commit.
type Interposed = Box<dyn FnOnce() + Send>;

/// A port that runs its interposed work the first time it is asked to swap
/// an envelope, before it swaps, as a writer whose commit lands between
/// another's read and its swap.
#[derive(Clone, Default)]
struct Overlapping {
    storage: MemoryStorage,
    interposed: Arc<Mutex<Option<Interposed>>>,
}

impl std::fmt::Debug for Overlapping {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Overlapping")
            .field(&self.storage)
            .finish()
    }
}

impl CollaborationStoragePort for Overlapping {
    fn source(&self, document: &CollaborationDocumentId) -> String {
        self.storage.source(document)
    }

    fn sources(&self, entity: &str) -> StoreResult<Vec<String>> {
        self.storage.sources(entity)
    }

    fn read(&self, source: &str) -> StoreResult<Option<StoredEnvelope>> {
        self.storage.read(source)
    }

    fn stamp(&self, source: &str) -> StoreResult<Option<String>> {
        self.storage.stamp(source)
    }

    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>> {
        let interposed = self.interposed.lock().unwrap().take();
        if let Some(interposed) = interposed {
            interposed();
        }
        self.storage.compare_and_swap(document, expected, bytes)
    }

    fn compare_and_remove(
        &self,
        document: &CollaborationDocumentId,
        expected: &str,
    ) -> StoreResult<bool> {
        self.storage.compare_and_remove(document, expected)
    }

    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<String> {
        self.storage.preserve(document, label, bytes)
    }

    fn append_audit(&self, record: &CollaborationAuditRecord) -> StoreResult<()> {
        self.storage.append_audit(record)
    }

    fn audit(&self, query: &CollaborationAuditQuery) -> StoreResult<Vec<CollaborationAuditRecord>> {
        self.storage.audit(query)
    }

    fn discard_audit_before(&self, unix_ms: u128) -> StoreResult<usize> {
        self.storage.discard_audit_before(unix_ms)
    }
}

/// A call a test makes of the store, given the note as seeded.
type Call = dyn Fn(&CollaborationService, &CollaborationDocumentRead) -> StoreResult<CollaborationImportResult>
    + Send
    + Sync;

/// Runs `call` on the seeded note twice at once: the second time between the
/// first's read of the note and its swap. Returns the first call's result,
/// the note it leaves, and everything announced.
fn overlapped(
    call: Arc<Call>,
) -> (
    StoreResult<CollaborationImportResult>,
    CollaborationDocumentRead,
    Vec<ChangeEvent>,
) {
    let port = Overlapping::default();
    let service = CollaborationService::new(port.clone(), PLANS, POLICY, key(), "notes");
    service
        .bootstrap(
            &NOTE_PLAN,
            &note(),
            &json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" }),
            accept,
        )
        .expect("bootstrap");
    let seed = read_note(&service);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let log = heard.clone();
    service
        .changes()
        .listen(move |change| log.lock().unwrap().push(change.clone()));
    let (inner, overlapping, inner_seed) = (service.clone(), call.clone(), seed.clone());
    *port.interposed.lock().unwrap() = Some(Box::new(move || {
        overlapping(&inner, &inner_seed).expect("the overlapping call");
    }));

    let outer = call(&service, &seed);

    let heard = heard.lock().unwrap().clone();
    (outer, read_note(&service), heard)
}

#[test]
fn an_edit_overlapping_its_own_retry_is_answered_as_a_duplicate() {
    let (outer, after, heard) = overlapped(Arc::new(|service, seed| {
        service.edit(
            &NOTE_PLAN,
            body_edit(seed, &writer(), "edit-body", "Edited."),
            accept,
        )
    }));

    let outer = outer.expect("the call the retry overlapped");
    assert!(outer.duplicate);
    assert_eq!(after.document["body"], "Edited.");
    assert_eq!(outer.etag, after.summary.etag);
    assert_eq!(heard.len(), 1, "one edit is committed");
}

#[test]
fn a_text_edit_overlapping_its_own_retry_is_answered_as_a_duplicate() {
    let (outer, after, heard) = overlapped(Arc::new(|service, seed| {
        service.edit_text(
            &NOTE_PLAN,
            text_edit(seed, &writer(), "writer-text", "Written."),
            accept,
        )
    }));

    let outer = outer.expect("the call the retry overlapped");
    assert!(outer.duplicate);
    assert_eq!(after.document["body"], "Written.");
    assert_eq!(outer.etag, after.summary.etag);
    assert_eq!(heard.len(), 1, "one edit is committed");
}

/// Refuses a body longer than 12 characters.
fn short(document: &Value) -> StoreResult<()> {
    match document["body"].as_str() {
        Some(body) if body.chars().count() > 12 => {
            Err(StoreError::invalid_request("The body is too long."))
        }
        _ => Ok(()),
    }
}

#[test]
fn a_text_edit_overlapping_its_own_retry_is_not_judged_merged_with_it() {
    let (outer, after, heard) = overlapped(Arc::new(|service, seed| {
        service.edit_text(
            &NOTE_PLAN,
            text_edit(seed, &writer(), "writer-text", "Written."),
            short,
        )
    }));

    let outer = outer.expect("the call the retry overlapped");
    assert!(outer.duplicate);
    assert_eq!(after.document["body"], "Written.");
    assert_eq!(heard.len(), 1, "one edit is committed");
}

/// The refusals `service` has audited, as the principal, operation id and
/// code of each.
fn audited_refusals(service: &CollaborationService) -> Vec<(Principal, String, String)> {
    service
        .audit(&CollaborationAuditQuery::default())
        .expect("audit")
        .into_iter()
        .filter_map(|record| match record.event {
            CollaborationAuditEvent::Refusal {
                operation_id, code, ..
            } => Some((record.actor, operation_id, code)),
            CollaborationAuditEvent::Recovery { .. } => None,
        })
        .collect()
}

#[test]
fn an_edit_refused_before_its_operations_are_prepared_is_audited() {
    let (service, _) = seeded();
    let read = read_note(&service);
    let mut absent = body_edit(&read, &writer(), "absent", "Edited.");
    absent.document = CollaborationDocumentId::new("Note", "note-2");
    let mut malformed = body_edit(&read, &writer(), "malformed", "Edited.");
    malformed.edited["body"] = json!(7);
    let mut misread = text_edit(&read, &operator(), "misread", "Written.");
    misread.read = "Not what was read.".to_owned();

    service
        .edit(&NOTE_PLAN, absent, accept)
        .expect_err("no such note");
    service
        .edit(&NOTE_PLAN, malformed, accept)
        .expect_err("a body that is not text");
    service
        .edit_text(&NOTE_PLAN, misread, accept)
        .expect_err("text other than what was read");

    assert_eq!(
        audited_refusals(&service),
        [
            (
                writer(),
                "absent".to_owned(),
                "collaboration_document_not_found".to_owned()
            ),
            (
                writer(),
                "malformed".to_owned(),
                "invalid_collaboration_update".to_owned()
            ),
            (
                operator(),
                "misread".to_owned(),
                "collaboration_text_mismatch".to_owned()
            ),
        ]
    );
}

#[test]
fn an_edit_refused_when_it_is_imported_is_audited_once() {
    let (service, _) = seeded();
    let stale = read_note(&service);
    service
        .edit(
            &NOTE_PLAN,
            body_edit(&stale, &operator(), "first", "First."),
            accept,
        )
        .expect("the first edit");

    service
        .edit(
            &NOTE_PLAN,
            body_edit(&stale, &writer(), "stale", "Second."),
            accept,
        )
        .expect_err("an edit of a document changed since it was read");

    assert_eq!(
        audited_refusals(&service),
        [(
            writer(),
            "stale".to_owned(),
            "collaboration_revision".to_owned()
        )]
    );
}

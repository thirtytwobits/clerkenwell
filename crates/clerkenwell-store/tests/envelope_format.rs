//! An envelope kept in an earlier format is refused until an explicit
//! upgrade rewrites it, and the upgrade changes nothing else.

mod support;

use clerkenwell_store::{
    upgrade_envelope_bytes, CollaborationDocumentId, CollaborationService,
    CollaborationStoragePort, LocalFileCollaborationStorage, ENVELOPE_VERSION,
};
use serde_json::{json, Value};
use support::{accept, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

/// A service over a fresh store holding one note, and where its envelope is.
fn seeded() -> (tempfile::TempDir, CollaborationService, String) {
    let root = tempfile::tempdir().expect("temp store");
    let service = CollaborationService::new(root.path(), PLANS, POLICY);
    service
        .bootstrap(
            &NOTE_PLAN,
            &note(),
            &json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" }),
            accept,
        )
        .expect("bootstrap");
    let path = LocalFileCollaborationStorage::new(root.path()).source(&note());
    (root, service, path)
}

/// Rewrites the envelope at `path` in format 1, which also kept where the
/// application rendered the document and whether it had published it.
fn keep_in_format_1(path: &str, earlier: &Value) {
    let mut envelope: serde_json::Map<String, Value> =
        serde_json::from_slice(&std::fs::read(path).expect("read")).expect("an envelope");
    envelope.insert("envelope_version".to_string(), json!(1));
    for (field, value) in earlier.as_object().expect("fields") {
        envelope.insert(field.clone(), value.clone());
    }
    std::fs::write(path, serde_json::to_vec_pretty(&envelope).expect("json")).expect("write");
}

fn format_1_fields() -> Value {
    json!({ "relative_path": "notes/note-1.yaml", "publication_pending": true })
}

fn refusal_code(error: &clerkenwell_store::StoreError) -> Option<&str> {
    error.data.as_ref().and_then(|data| data["code"].as_str())
}

#[test]
fn an_envelope_in_an_earlier_format_is_described_but_its_content_refused() {
    let (_root, service, path) = seeded();
    keep_in_format_1(&path, &format_1_fields());
    let kept = std::fs::read(&path).expect("read");

    let refused = service.load(&note()).expect_err("an earlier format");
    assert_eq!(
        refusal_code(&refused),
        Some("collaboration_envelope_upgrade_required")
    );
    assert!(service.detail(&NOTE_PLAN, &note()).is_err());
    let inspection = service.verify(&note());
    assert!(!inspection.valid);
    assert_eq!(
        inspection.failure_code.as_deref(),
        Some("collaboration_envelope_upgrade_required")
    );
    let described = service
        .summary(&note())
        .expect("summary")
        .expect("a document");
    assert!(described.envelope_version < ENVELOPE_VERSION);
    assert_eq!(service.summaries("Note").expect("summaries"), [described]);
    let moved = service
        .move_document(&note(), &CollaborationDocumentId::new("Note", "note-2"))
        .expect_err("an earlier format");
    assert_eq!(
        refusal_code(&moved),
        Some("collaboration_envelope_upgrade_required")
    );

    assert_eq!(std::fs::read(&path).expect("read"), kept);
}

#[test]
fn an_upgrade_returns_what_the_earlier_format_kept_and_leaves_the_document_as_it_was() {
    let (_root, service, path) = seeded();
    let before = service.load(&note()).expect("load").expect("an envelope");
    let earlier = format_1_fields();
    keep_in_format_1(&path, &earlier);

    let removed = service
        .upgrade_envelope(&note())
        .expect("upgrade")
        .expect("an earlier format");

    assert_eq!(Value::Object(removed), earlier);
    let after = service.load(&note()).expect("load").expect("an envelope");
    assert_eq!(after.envelope_version, ENVELOPE_VERSION);
    assert_eq!(
        service
            .summary(&note())
            .expect("summary")
            .expect("a document")
            .envelope_version,
        ENVELOPE_VERSION
    );
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.etag(), before.etag());
    assert_eq!(
        service
            .detail(&NOTE_PLAN, &note())
            .expect("detail")
            .expect("a note")["body"],
        "Seeded."
    );
}

#[test]
fn upgrading_an_envelope_already_in_the_current_format_changes_nothing() {
    let (_root, service, path) = seeded();
    let kept = std::fs::read(&path).expect("read");

    assert!(service
        .upgrade_envelope(&note())
        .expect("upgrade")
        .is_none());

    assert_eq!(std::fs::read(&path).expect("read"), kept);
}

#[test]
fn envelope_bytes_no_service_reads_upgrade_to_what_a_service_reads() {
    let (root, service, path) = seeded();
    let before = service.load(&note()).expect("load").expect("an envelope");
    let earlier = format_1_fields();
    keep_in_format_1(&path, &earlier);
    drop(service);

    let (upgraded, removed) = upgrade_envelope_bytes(&std::fs::read(&path).expect("read"))
        .expect("upgrade")
        .expect("an earlier format");
    std::fs::write(&path, upgraded).expect("write");

    assert_eq!(Value::Object(removed), earlier);
    let reader = CollaborationService::new(root.path(), PLANS, POLICY);
    let after = reader.load(&note()).expect("load").expect("an envelope");
    assert_eq!(after.envelope_version, ENVELOPE_VERSION);
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.etag(), before.etag());
}

#[test]
fn envelope_bytes_already_in_the_current_format_are_left_as_they_are() {
    let (_root, _service, path) = seeded();

    assert!(upgrade_envelope_bytes(&std::fs::read(&path).expect("read"))
        .expect("upgrade")
        .is_none());
}

#[test]
fn envelope_bytes_in_a_format_this_store_does_not_know_are_refused() {
    let (_root, _service, path) = seeded();
    let mut envelope: serde_json::Map<String, Value> =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("an envelope");
    envelope.insert("envelope_version".to_string(), json!(ENVELOPE_VERSION + 1));

    let refused = upgrade_envelope_bytes(&serde_json::to_vec(&envelope).expect("json"))
        .expect_err("an unknown format");

    assert_eq!(refusal_code(&refused), Some("collaboration_state_corrupt"));
}

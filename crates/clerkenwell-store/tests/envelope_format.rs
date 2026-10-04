//! The store reads and writes envelopes in one format, and refuses an envelope
//! in any other as corrupt without changing it.

mod support;

use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationService, CollaborationStoragePort, ENVELOPE_VERSION,
};
use serde_json::{json, Value};
use support::{accept, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

fn service(storage: &MemoryStorage) -> CollaborationService {
    CollaborationService::new(storage.clone(), PLANS, POLICY, "notes")
}

/// A fresh store holding one note.
fn seeded() -> MemoryStorage {
    let storage = MemoryStorage::default();
    service(&storage)
        .bootstrap(
            &NOTE_PLAN,
            &note(),
            &json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" }),
            accept,
        )
        .expect("bootstrap");
    storage
}

/// The note's stored bytes.
fn stored_bytes(storage: &MemoryStorage) -> Vec<u8> {
    storage
        .read(&storage.source(&note()))
        .expect("read")
        .expect("stored")
        .bytes
}

fn stored_envelope(storage: &MemoryStorage) -> serde_json::Map<String, Value> {
    serde_json::from_slice(&stored_bytes(storage)).expect("an envelope")
}

/// Rewrites the note's envelope to declare `format`.
fn declare_format(storage: &MemoryStorage, format: u32) {
    let stored = storage
        .read(&storage.source(&note()))
        .expect("read")
        .expect("stored");
    let mut envelope = stored_envelope(storage);
    envelope.insert("envelope_version".to_string(), json!(format));
    storage
        .compare_and_swap(
            &note(),
            Some(&stored.version),
            &serde_json::to_vec_pretty(&envelope).expect("json"),
        )
        .expect("write")
        .expect("unchanged since read");
}

fn refusal_code(error: &clerkenwell_store::StoreError) -> Option<&str> {
    error.data.as_ref().and_then(|data| data["code"].as_str())
}

#[test]
fn an_envelope_the_store_writes_declares_its_format_and_reads_back() {
    let storage = seeded();

    assert_eq!(
        stored_envelope(&storage)["envelope_version"],
        json!(ENVELOPE_VERSION)
    );
    let reader = service(&storage);
    assert_eq!(
        reader
            .detail(&NOTE_PLAN, &note())
            .expect("detail")
            .expect("a note")["body"],
        "Seeded."
    );
}

#[test]
fn an_envelope_in_another_format_is_refused_as_corrupt_and_left_as_it_is() {
    for format in [ENVELOPE_VERSION - 1, ENVELOPE_VERSION + 1] {
        let storage = seeded();
        declare_format(&storage, format);
        let kept = stored_bytes(&storage);
        let service = service(&storage);

        let refused = service
            .detail(&NOTE_PLAN, &note())
            .expect_err("another format");
        assert_eq!(
            refusal_code(&refused),
            Some("collaboration_state_corrupt"),
            "format {format}"
        );
        let refused = service.summary(&note()).expect_err("another format");
        assert_eq!(
            refusal_code(&refused),
            Some("collaboration_state_corrupt"),
            "format {format}"
        );
        assert!(
            service.summaries("Note").is_err(),
            "format {format} is summarised"
        );
        let verified = service.verify(&note());
        assert!(!verified.valid, "format {format} verifies");
        assert_eq!(
            verified.failure_code.as_deref(),
            Some("collaboration_state_corrupt"),
            "format {format}"
        );
        let (inspected, _) = service.inspect(Some("Note"), None, None).expect("inspect");
        assert!(
            inspected.iter().all(|inspection| !inspection.valid
                && inspection.failure_code.as_deref() == Some("collaboration_state_corrupt")),
            "format {format}"
        );
        assert_eq!(inspected.len(), 1, "format {format}");
        let refused = service
            .repair(&note(), "another format")
            .expect_err("another format");
        assert_eq!(
            refusal_code(&refused),
            Some("collaboration_state_corrupt"),
            "format {format}"
        );

        assert_eq!(stored_bytes(&storage), kept, "format {format}");
    }
}

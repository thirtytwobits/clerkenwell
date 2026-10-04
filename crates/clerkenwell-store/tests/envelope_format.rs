//! The store reads and writes envelopes in one format, and refuses an envelope
//! in any other as corrupt without changing it.

mod support;

use clerkenwell_store::{
    CollaborationDocumentId, CollaborationService, CollaborationStoragePort,
    LocalFileCollaborationStorage, ENVELOPE_VERSION,
};
use serde_json::{json, Value};
use support::{accept, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

/// A fresh store holding one note, and where its envelope is.
fn seeded() -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().expect("temp store");
    CollaborationService::new(root.path(), PLANS, POLICY)
        .bootstrap(
            &NOTE_PLAN,
            &note(),
            &json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" }),
            accept,
        )
        .expect("bootstrap");
    let path = LocalFileCollaborationStorage::new(root.path()).source(&note());
    (root, path)
}

fn stored_envelope(path: &str) -> serde_json::Map<String, Value> {
    serde_json::from_slice(&std::fs::read(path).expect("read")).expect("an envelope")
}

/// Rewrites the envelope at `path` to declare `format`.
fn declare_format(path: &str, format: u32) {
    let mut envelope = stored_envelope(path);
    envelope.insert("envelope_version".to_string(), json!(format));
    std::fs::write(path, serde_json::to_vec_pretty(&envelope).expect("json")).expect("write");
}

fn refusal_code(error: &clerkenwell_store::StoreError) -> Option<&str> {
    error.data.as_ref().and_then(|data| data["code"].as_str())
}

#[test]
fn an_envelope_the_store_writes_declares_its_format_and_reads_back() {
    let (root, path) = seeded();

    assert_eq!(
        stored_envelope(&path)["envelope_version"],
        json!(ENVELOPE_VERSION)
    );
    let reader = CollaborationService::new(root.path(), PLANS, POLICY);
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
        let (root, path) = seeded();
        declare_format(&path, format);
        let kept = std::fs::read(&path).expect("read");
        let service = CollaborationService::new(root.path(), PLANS, POLICY);

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

        assert_eq!(std::fs::read(&path).expect("read"), kept, "format {format}");
    }
}

//! A validator's own error reaches the caller unchanged.

mod support;

use clerkenwell_store::{
    CollaborationDocumentId, CollaborationService, CollaborationStoragePort, StoreError,
};
use serde_json::{json, Value};
use support::{NOTE_PLAN, PLANS};

#[derive(Debug)]
enum AppError {
    Store(StoreError),
    Rejected(String),
}

impl From<StoreError> for AppError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

#[test]
fn a_rejecting_validator_fails_the_commit_with_its_own_error() {
    let root = tempfile::tempdir().expect("temp store");
    let service = CollaborationService::new(root.path(), PLANS);
    let document = CollaborationDocumentId::new("Note", "note-1");
    let reason = "notes must not be empty";
    let seed = json!({ "note_id": "note-1", "body": "", "etag": "" });

    let refused = service.bootstrap(&NOTE_PLAN, &document, "note-1", &seed, |note: &Value| {
        if note["body"].as_str().is_some_and(str::is_empty) {
            Err(AppError::Rejected(reason.to_string()))
        } else {
            Ok(())
        }
    });

    match refused {
        Err(AppError::Rejected(message)) => assert_eq!(message, reason),
        Err(AppError::Store(error)) => panic!("expected the validator's rejection, got {error}"),
        Ok(envelope) => panic!("expected the validator's rejection, got {envelope:?}"),
    }
    assert!(service
        .storage()
        .load(&document)
        .expect("read store")
        .is_none());
}

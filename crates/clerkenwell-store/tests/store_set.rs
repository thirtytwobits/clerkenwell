//! An application keeps its documents in a set of named stores.

mod support;

use std::sync::{Arc, Mutex};

use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationRecoveryAuditRecord, CollaborationStoragePort, CollaborationStores, ImportFence,
    LocalFileCollaborationStorage, StoreResult, StoredEnvelope,
};
use serde_json::{json, Value};
use support::{accept, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

fn seed() -> Value {
    json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" })
}

#[test]
fn a_name_the_set_holds_is_the_store_it_keeps() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stores = CollaborationStores::new(PLANS, POLICY);
    let first =
        stores.store_or_register("tasks", || LocalFileCollaborationStorage::new(one.path()));
    let again =
        stores.store_or_register("tasks", || LocalFileCollaborationStorage::new(two.path()));

    first
        .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
        .expect("bootstrap");
    assert!(again.summary(&note()).expect("summary").is_some());
    assert_eq!(stores.stores().len(), 1);
}

#[test]
fn registering_a_name_the_set_holds_is_refused() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stores = CollaborationStores::new(PLANS, POLICY);
    stores
        .register("tasks", LocalFileCollaborationStorage::new(one.path()))
        .expect("register");

    assert!(stores
        .register("tasks", LocalFileCollaborationStorage::new(two.path()))
        .is_err());
    let names: Vec<String> = stores.stores().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["tasks"]);
}

/// A storage port of a kind other than the file port, keeping its bytes
/// through one.
#[derive(Debug)]
struct Relayed(LocalFileCollaborationStorage);

impl CollaborationStoragePort for Relayed {
    fn source(&self, document: &CollaborationDocumentId) -> String {
        self.0.source(document)
    }
    fn sources(&self, entity: &str) -> StoreResult<Vec<String>> {
        self.0.sources(entity)
    }
    fn read(&self, source: &str) -> StoreResult<Option<StoredEnvelope>> {
        self.0.read(source)
    }
    fn stamp(&self, source: &str) -> StoreResult<Option<String>> {
        self.0.stamp(source)
    }
    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>> {
        self.0.compare_and_swap(document, expected, bytes)
    }
    fn compare_and_remove(
        &self,
        document: &CollaborationDocumentId,
        expected: &str,
    ) -> StoreResult<bool> {
        self.0.compare_and_remove(document, expected)
    }
    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<String> {
        self.0.preserve(document, label, bytes)
    }
    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()> {
        self.0.append_recovery_audit(record)
    }
    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        self.0.recovery_audit()
    }
}

#[test]
fn one_set_holds_stores_kept_by_different_kinds_of_storage() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stores = CollaborationStores::new(PLANS, POLICY);
    let files = stores
        .register("files", LocalFileCollaborationStorage::new(one.path()))
        .expect("register");
    let relayed = stores
        .register(
            "relayed",
            Relayed(LocalFileCollaborationStorage::new(two.path())),
        )
        .expect("register");

    for store in [&files, &relayed] {
        store
            .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
            .expect("bootstrap");
    }

    for name in ["files", "relayed"] {
        let store = stores.store(name).expect("registered");
        assert!(store.summary(&note()).expect("summary").is_some(), "{name}");
    }
}

#[test]
fn the_set_holds_each_store_until_it_is_forgotten() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stores = CollaborationStores::new(PLANS, POLICY);
    stores
        .register("second", LocalFileCollaborationStorage::new(two.path()))
        .expect("register");
    stores
        .register("first", LocalFileCollaborationStorage::new(one.path()))
        .expect("register");
    let names: Vec<String> = stores.stores().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["first", "second"]);

    assert!(stores.forget("first"));
    assert!(stores.store("first").is_none());
    assert!(stores.store("second").is_some());
    assert!(!stores.forget("first"));
}

#[test]
fn every_store_announces_on_the_sets_feed_under_its_own_name() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stores = CollaborationStores::new(PLANS, POLICY);
    let sources = Arc::new(Mutex::new(Vec::new()));
    let heard = sources.clone();
    stores
        .changes()
        .listen(move |change| heard.lock().unwrap().push(change.source.clone()));

    for (name, root) in [("first", one.path()), ("second", two.path())] {
        stores
            .register(name, LocalFileCollaborationStorage::new(root))
            .expect("register")
            .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
            .expect("bootstrap");
    }
    assert_eq!(*sources.lock().unwrap(), ["first", "second"]);
}

#[test]
fn the_set_counts_what_every_store_in_it_counts() {
    let root = tempfile::tempdir().unwrap();
    let stores = CollaborationStores::new(PLANS, POLICY);
    let store = stores
        .register("tasks", LocalFileCollaborationStorage::new(root.path()))
        .expect("register");
    store
        .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
        .expect("bootstrap");
    let state = store
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    let request = CollaborationImportRequest {
        document: note(),
        schema_version: NOTE_PLAN.schema_version,
        operation_id: "resent".to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: state.accepted_frontier_base64.clone(),
        update_base64: state.update_base64.clone(),
        fence: ImportFence::Frontier,
    };
    let before = stores.counters().duplicate_imports;
    for _ in 0..2 {
        store
            .import(&NOTE_PLAN, request.clone(), accept)
            .expect("import");
    }
    assert!(stores.counters().duplicate_imports > before);
    assert_eq!(stores.counters(), store.counters());
}

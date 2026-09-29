//! An application keeps its documents in a set of named stores.

mod support;

use std::sync::{Arc, Mutex};

use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationStores, ImportFence,
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
fn a_name_registered_again_for_its_root_is_the_same_store() {
    let root = tempfile::tempdir().expect("temp store");
    let stores = CollaborationStores::new(PLANS, POLICY);
    let first = stores.register("tasks", root.path()).expect("register");
    let again = stores
        .register("tasks", root.path())
        .expect("register again");

    first
        .bootstrap(&NOTE_PLAN, &note(), "notes/note-1.yaml", &seed(), accept)
        .expect("bootstrap");
    assert!(again.load(&note()).expect("load").is_some());
    assert_eq!(stores.stores().len(), 1);
}

#[test]
fn a_name_registered_for_another_root_is_refused() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stores = CollaborationStores::new(PLANS, POLICY);
    stores.register("tasks", one.path()).expect("register");

    assert!(stores.register("tasks", two.path()).is_err());
    let names: Vec<String> = stores.stores().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["tasks"]);
}

#[test]
fn the_set_holds_each_store_until_it_is_forgotten() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let stores = CollaborationStores::new(PLANS, POLICY);
    stores.register("second", two.path()).expect("register");
    stores.register("first", one.path()).expect("register");
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
            .register(name, root)
            .expect("register")
            .bootstrap(&NOTE_PLAN, &note(), "notes/note-1.yaml", &seed(), accept)
            .expect("bootstrap");
    }
    assert_eq!(*sources.lock().unwrap(), ["first", "second"]);
}

#[test]
fn the_set_counts_what_every_store_in_it_counts() {
    let root = tempfile::tempdir().unwrap();
    let stores = CollaborationStores::new(PLANS, POLICY);
    let store = stores.register("tasks", root.path()).expect("register");
    store
        .bootstrap(&NOTE_PLAN, &note(), "notes/note-1.yaml", &seed(), accept)
        .expect("bootstrap");
    let state = store
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    let request = CollaborationImportRequest {
        document: note(),
        relative_path: "notes/note-1.yaml".to_string(),
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

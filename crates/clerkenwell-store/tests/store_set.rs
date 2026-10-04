//! An application keeps its documents in a set of named stores.

mod support;

use std::sync::{Arc, Mutex};

use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationStores, ImportFence,
};
use serde_json::{json, Value};
use support::{accept, Renamed, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

fn seed() -> Value {
    json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" })
}

#[test]
fn a_name_the_set_holds_is_the_store_it_keeps() {
    let stores = CollaborationStores::new(PLANS, POLICY);
    let first = stores.store_or_register("tasks", MemoryStorage::default);
    let again = stores.store_or_register("tasks", MemoryStorage::default);

    first
        .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
        .expect("bootstrap");
    assert!(again.summary(&note()).expect("summary").is_some());
    assert_eq!(stores.stores().len(), 1);
}

#[test]
fn registering_a_name_the_set_holds_is_refused() {
    let stores = CollaborationStores::new(PLANS, POLICY);
    stores
        .register("tasks", MemoryStorage::default())
        .expect("register");

    assert!(stores.register("tasks", MemoryStorage::default()).is_err());
    let names: Vec<String> = stores.stores().into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["tasks"]);
}

#[test]
fn one_set_holds_stores_kept_by_different_kinds_of_storage() {
    let stores = CollaborationStores::new(PLANS, POLICY);
    let memory = stores
        .register("memory", MemoryStorage::default())
        .expect("register");
    let renamed = stores
        .register("renamed", Renamed::default())
        .expect("register");

    for store in [&memory, &renamed] {
        store
            .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
            .expect("bootstrap");
    }

    for name in ["memory", "renamed"] {
        let store = stores.store(name).expect("registered");
        assert!(store.summary(&note()).expect("summary").is_some(), "{name}");
    }
}

#[test]
fn the_set_holds_each_store_until_it_is_forgotten() {
    let stores = CollaborationStores::new(PLANS, POLICY);
    stores
        .register("second", MemoryStorage::default())
        .expect("register");
    stores
        .register("first", MemoryStorage::default())
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
    let stores = CollaborationStores::new(PLANS, POLICY);
    let sources = Arc::new(Mutex::new(Vec::new()));
    let heard = sources.clone();
    stores
        .changes()
        .listen(move |change| heard.lock().unwrap().push(change.source.clone()));

    for name in ["first", "second"] {
        stores
            .register(name, MemoryStorage::default())
            .expect("register")
            .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
            .expect("bootstrap");
    }
    assert_eq!(*sources.lock().unwrap(), ["first", "second"]);
}

#[test]
fn the_set_counts_what_every_store_in_it_counts() {
    let stores = CollaborationStores::new(PLANS, POLICY);
    let store = stores
        .register("tasks", MemoryStorage::default())
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

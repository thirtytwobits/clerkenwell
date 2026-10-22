//! A store reports the version of each document it holds: the document's
//! incarnation, and how far into that history it has accepted. Versions
//! compare only within one incarnation, and a peer holding a version is sent
//! the operations it lacks.

mod support;

use std::collections::BTreeMap;

use clerkenwell_doc::CollaborationReplica;
use clerkenwell_schema::{CollaborationExchangeMode, GeneratedCollaborationEntitySpec};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationHeld, CollaborationImportResult, CollaborationService,
    CollaborationVersion, ImportFence, PeerBlock, StoreError,
};
use serde_json::{json, Value};
use support::{accept, client, key, operator, request, writer, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

fn seed() -> Value {
    json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" })
}

fn service() -> CollaborationService {
    CollaborationService::new(MemoryStorage::default(), PLANS, POLICY, key(), "notes")
}

fn seeded() -> CollaborationService {
    let service = service();
    service
        .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
        .expect("bootstrap");
    service
}

fn version(service: &CollaborationService) -> CollaborationVersion {
    service
        .summary(&note())
        .expect("summary")
        .expect("a note")
        .version
}

fn body(service: &CollaborationService) -> Value {
    service
        .detail(&NOTE_PLAN, &note())
        .expect("detail")
        .expect("a note")["body"]
        .clone()
}

/// Imports an edit setting the note's body, from what `service` reads now.
fn edit(service: &CollaborationService, body: &str) -> CollaborationImportResult {
    let state = service
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    let (mut client, block) = client(service, &note(), &writer(), &state);
    let mut edited = client.materialized_document("edit").expect("document");
    edited["body"] = Value::from(body);
    client.replace_document(&edited).expect("edit");
    service
        .import(
            &NOTE_PLAN,
            request(
                &note(),
                &writer(),
                &block,
                &format!("edit-{body}"),
                &state.accepted_frontier_base64,
                client
                    .export_incremental_update_base64(&state.accepted_frontier_base64)
                    .expect("update"),
            ),
            accept,
        )
        .expect("import")
}

/// A replica of the note as the authoring state the store sends with
/// nothing held delivers it now.
fn replica_now(service: &CollaborationService) -> CollaborationReplica {
    let state = service
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("replica")
}

fn refusal_code(error: &StoreError) -> Option<&str> {
    error.data.as_ref().and_then(|data| data["code"].as_str())
}

#[test]
fn a_commit_reports_the_version_it_leaves_accepted() {
    let service = seeded();
    let seeded = version(&service);

    let imported = edit(&service, "Edited.");

    assert_eq!(imported.version, version(&service));
    assert_eq!(
        service
            .read_document(&NOTE_PLAN, &note())
            .expect("read")
            .expect("a note")
            .summary
            .version,
        imported.version
    );
    assert!(imported.version.covers(&seeded));
    assert!(!seeded.covers(&imported.version));
}

#[test]
fn a_document_keeps_its_incarnation_across_commits() {
    let service = seeded();
    let seeded = version(&service);
    edit(&service, "Edited.");
    edit(&service, "Edited again.");

    assert_eq!(version(&service).incarnation, seeded.incarnation);
}

#[test]
fn a_retried_import_reports_the_version_its_first_commit_left() {
    let service = seeded();
    let state = service
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    let (mut client, block) = client(&service, &note(), &writer(), &state);
    let mut edited = client.materialized_document("edit").expect("document");
    edited["body"] = Value::from("Edited.");
    client.replace_document(&edited).expect("edit");
    let send = || {
        service
            .import(
                &NOTE_PLAN,
                request(
                    &note(),
                    &writer(),
                    &block,
                    "edit",
                    &state.accepted_frontier_base64,
                    client
                        .export_incremental_update_base64(&state.accepted_frontier_base64)
                        .expect("update"),
                ),
                accept,
            )
            .expect("import")
    };

    let first = send();
    let retried = send();

    assert!(retried.duplicate);
    assert_eq!(retried.version, first.version);
}

#[test]
fn removing_a_document_reports_the_version_it_removed() {
    let service = seeded();
    edit(&service, "Edited.");
    let accepted = version(&service);

    let removed = service.delete(&operator(), &note()).expect("delete");

    assert_eq!(removed, Some(accepted));
    assert_eq!(
        service.delete(&operator(), &note()).expect("delete again"),
        None
    );
}

#[test]
fn a_document_created_again_is_another_incarnation() {
    let service = seeded();
    let removed = version(&service);
    service.delete(&operator(), &note()).expect("delete");

    service
        .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
        .expect("bootstrap again");
    let created = version(&service);

    assert_ne!(created.incarnation, removed.incarnation);
    assert!(!created.covers(&removed));
    assert!(!removed.covers(&created));
}

/// Creates the note with a bootstrap import of `replica`'s history, written
/// under `block`.
fn create_by_import(
    service: &CollaborationService,
    replica: &CollaborationReplica,
    block: &PeerBlock,
    operation_id: &str,
) -> CollaborationImportResult {
    let mut created = request(
        &note(),
        &writer(),
        block,
        operation_id,
        "",
        replica.export_update_base64().expect("update"),
    );
    created.exchange_mode = CollaborationExchangeMode::Bootstrap;
    created.fence = ImportFence::Frontier;
    service
        .import(&NOTE_PLAN, created, accept)
        .expect("bootstrap import")
}

#[test]
fn a_writer_keeping_its_block_across_a_removal_writes_another_incarnation() {
    let service = service();
    let block = service.allocate_peers(&writer(), &note());
    let first = CollaborationReplica::from_document(&NOTE_PLAN, &seed(), block.base)
        .expect("first history");
    let removed = create_by_import(&service, &first, &block, "create-first").version;
    service.delete(&operator(), &note()).expect("delete");

    // The same peer writes another history with as many operations: a body
    // of as many characters.
    let mut second = seed();
    second["body"] = Value::from("Renewed");
    assert_eq!(
        second["body"].as_str().map(str::len),
        seed()["body"].as_str().map(str::len)
    );
    let second = CollaborationReplica::from_document(&NOTE_PLAN, &second, block.base)
        .expect("second history");
    let created = create_by_import(&service, &second, &block, "create-second").version;

    assert_eq!(
        created.peers, removed.peers,
        "the vectors alone cannot tell the histories apart"
    );
    assert_ne!(created.incarnation, removed.incarnation);
    assert!(!created.covers(&removed));
    assert!(!removed.covers(&created));
}

#[test]
fn a_document_created_by_import_reports_the_version_it_holds() {
    let service = service();
    let block = service.allocate_peers(&writer(), &note());
    let replica =
        CollaborationReplica::from_document(&NOTE_PLAN, &seed(), block.base).expect("history");

    let created = create_by_import(&service, &replica, &block, "create");

    assert_eq!(created.version, version(&service));
}

#[test]
fn adopting_a_history_with_its_incarnation_reproduces_its_version() {
    let origin = seeded();
    edit(&origin, "Edited.");
    let held = version(&origin);
    let whole = origin
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");

    let adopter = service();
    let adopted = adopter
        .bootstrap_update(
            &NOTE_PLAN,
            &note(),
            whole.schema_version,
            &whole.update_base64,
            Some(&held.incarnation),
            accept,
        )
        .expect("adopt");

    assert_eq!(adopted.version, held);
    assert_eq!(body(&adopter), body(&origin));
}

#[test]
fn a_history_created_without_an_incarnation_gets_a_new_one() {
    let origin = seeded();
    let held = version(&origin);
    let whole = origin
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");

    let created = service()
        .bootstrap_update(
            &NOTE_PLAN,
            &note(),
            whole.schema_version,
            &whole.update_base64,
            None,
            accept,
        )
        .expect("create");

    assert_ne!(created.version.incarnation, held.incarnation);
    assert_eq!(created.version.peers, held.peers);
}

#[test]
fn an_empty_incarnation_is_refused() {
    let origin = seeded();
    let whole = origin
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    let adopter = service();

    let refused = adopter
        .bootstrap_update(
            &NOTE_PLAN,
            &note(),
            whole.schema_version,
            &whole.update_base64,
            Some(" "),
            accept,
        )
        .expect_err("an empty incarnation");

    assert_eq!(
        refusal_code(&refused),
        Some("invalid_collaboration_incarnation")
    );
    assert!(adopter.summary(&note()).expect("summary").is_none());
}

#[test]
fn a_migration_gives_the_document_a_new_incarnation() {
    let service = seeded();
    let before = version(&service);
    let migrated_plan: &'static GeneratedCollaborationEntitySpec =
        Box::leak(Box::new(GeneratedCollaborationEntitySpec {
            schema_version: NOTE_PLAN.schema_version + 1,
            ..NOTE_PLAN
        }));
    let rewritten = CollaborationReplica::from_document(
        migrated_plan,
        &seed(),
        service.allocate_peers(&operator(), &note()).base,
    )
    .expect("rewritten history");

    let migrated = service
        .migrate(
            migrated_plan,
            &note(),
            NOTE_PLAN.schema_version,
            &rewritten.export_update_base64().expect("update"),
            accept,
        )
        .expect("migrate");

    assert_eq!(migrated.version, version(&service));
    assert_ne!(migrated.version.incarnation, before.incarnation);
    assert!(!migrated.version.covers(&before));
}

#[test]
fn a_peer_holding_a_version_is_sent_the_operations_it_lacks() {
    let service = seeded();
    let held = version(&service);
    let mut peer = replica_now(&service);
    edit(&service, "Edited.");
    edit(&service, "Edited again.");

    let state = service
        .authoring_state(&NOTE_PLAN, &note(), Some(CollaborationHeld::Version(&held)))
        .expect("authoring state");

    assert!(!state.replaces_held);
    assert_eq!(state.version, version(&service));
    peer.adopt_versioned_update_base64(state.schema_version, &state.update_base64)
        .expect("adopt what it lacks");
    assert_eq!(
        peer.materialized_document("peer").expect("document")["body"],
        body(&service)
    );
}

#[test]
fn a_peer_holding_the_accepted_version_lacks_nothing() {
    let service = seeded();
    edit(&service, "Edited.");
    let held = version(&service);

    let state = service
        .authoring_state(&NOTE_PLAN, &note(), Some(CollaborationHeld::Version(&held)))
        .expect("authoring state");

    assert!(!state.replaces_held);
    assert!(
        CollaborationReplica::version_of_update(&state.update_base64)
            .expect("version")
            .is_empty()
    );
}

#[test]
fn a_version_naming_operations_the_store_lacks_is_answered_with_those_it_holds() {
    let service = seeded();
    let mut held = version(&service);
    let mut peer = replica_now(&service);
    edit(&service, "Edited.");
    // Another store's engine wrote operations this store has not seen.
    let unseen = u64::MAX >> 1;
    held.peers.insert(unseen, 12);

    let state = service
        .authoring_state(&NOTE_PLAN, &note(), Some(CollaborationHeld::Version(&held)))
        .expect("authoring state");

    assert!(!state.replaces_held);
    peer.adopt_versioned_update_base64(state.schema_version, &state.update_base64)
        .expect("adopt what it lacks");
    assert_eq!(
        peer.materialized_document("peer").expect("document")["body"],
        body(&service)
    );
}

#[test]
fn a_version_of_another_incarnation_is_answered_with_the_whole_history() {
    let service = seeded();
    edit(&service, "Edited.");
    let stale = CollaborationVersion {
        incarnation: "another".to_string(),
        ..version(&service)
    };

    let state = service
        .authoring_state(
            &NOTE_PLAN,
            &note(),
            Some(CollaborationHeld::Version(&stale)),
        )
        .expect("authoring state");

    assert!(state.replaces_held);
    let replaced = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("the whole history");
    assert_eq!(
        replaced.materialized_document("peer").expect("document")["body"],
        body(&service)
    );
}

#[test]
fn a_version_naming_a_counter_no_operation_reaches_is_refused() {
    let service = seeded();
    let held = CollaborationVersion {
        peers: BTreeMap::from([(1, u64::MAX)]),
        ..version(&service)
    };

    let refused = service
        .authoring_state(&NOTE_PLAN, &note(), Some(CollaborationHeld::Version(&held)))
        .expect_err("a counter beyond any operation");

    assert_eq!(
        refusal_code(&refused),
        Some("invalid_collaboration_version")
    );
}

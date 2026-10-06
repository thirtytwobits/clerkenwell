//! An import never adds a root to a document's history: a client holding a
//! history built apart from the store's, such as one a re-seed leaves it, is
//! told to resynchronise, and the document is unchanged.

mod support;

use clerkenwell_doc::CollaborationReplica;
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationImportRequest, CollaborationService, ImportFence,
    StoreError,
};
use loro::{ExportMode, LoroDoc};
use serde_json::{json, Value};
use support::{accept, client, key, request, writer, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

fn seed() -> Value {
    json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" })
}

fn seeded() -> CollaborationService {
    let service =
        CollaborationService::new(MemoryStorage::default(), PLANS, POLICY, key(), "notes");
    service
        .bootstrap(&NOTE_PLAN, &note(), &seed(), accept)
        .expect("bootstrap");
    service
}

fn decode(update: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(update)
        .expect("base64")
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// An import of a client's edit to its own seeding of the note, made under a
/// block allocated to the writer, after it took the store's history into its
/// replica without checking: what a re-seed leaves a client sending.
fn reseeded_client_edit(service: &CollaborationService) -> CollaborationImportRequest {
    let state = service
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    let block = service.allocate_peers(&writer(), &note());
    let mut own = CollaborationReplica::from_document(&NOTE_PLAN, &seed(), block.base)
        .expect("the client's own seeding");
    let mut edited = own.materialized_document("edit").expect("document");
    edited["body"] = json!("Seeded. Edited offline.");
    own.replace_document(&edited).expect("edit");
    let both = LoroDoc::new();
    for update in [
        own.export_update_base64().expect("export"),
        state.update_base64.clone(),
    ] {
        both.import(&decode(&update)).expect("import");
    }
    let both = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        NOTE_PLAN.schema_version,
        &encode(&both.export(ExportMode::all_updates()).expect("export")),
    )
    .expect("both histories");
    request(
        &note(),
        &writer(),
        &block,
        "reseeded-edit",
        &state.accepted_frontier_base64,
        both.export_incremental_update_base64(&state.accepted_frontier_base64)
            .expect("what follows the store's frontier"),
    )
}

fn refusal_code(error: &StoreError) -> Option<&str> {
    error.data.as_ref().and_then(|data| data["code"].as_str())
}

fn conflict_kind(error: &StoreError) -> Option<&str> {
    error
        .data
        .as_ref()
        .and_then(|data| data["conflict_kind"].as_str())
}

fn assert_refused_unchanged(service: &CollaborationService, request: CollaborationImportRequest) {
    let before = service.summary(&note()).expect("summary");
    let refused = service
        .import(&NOTE_PLAN, request, accept)
        .expect_err("a history built apart from the store's");
    assert_eq!(refusal_code(&refused), Some("conflict"));
    assert_eq!(
        conflict_kind(&refused),
        Some("collaboration_resync_required")
    );
    assert_eq!(service.summary(&note()).expect("summary"), before);
    assert_eq!(
        service
            .detail(&NOTE_PLAN, &note())
            .expect("detail")
            .expect("the note")["body"],
        "Seeded."
    );
}

#[test]
fn a_reseeded_clients_edit_fenced_on_the_stores_frontier_is_refused() {
    let service = seeded();
    let request = reseeded_client_edit(&service);
    assert_refused_unchanged(&service, request);
}

#[test]
fn a_reseeded_clients_edit_fenced_on_the_stores_etag_is_refused() {
    let service = seeded();
    let mut request = reseeded_client_edit(&service);
    let etag = service
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state")
        .etag;
    request.fence = ImportFence::Etag(etag);
    assert_refused_unchanged(&service, request);
}

#[test]
fn a_separately_built_history_sent_on_an_empty_base_is_refused() {
    let service = seeded();
    let block = service.allocate_peers(&writer(), &note());
    let own = CollaborationReplica::from_document(&NOTE_PLAN, &seed(), block.base)
        .expect("the client's own seeding");
    let request = request(
        &note(),
        &writer(),
        &block,
        "separate-seed",
        "",
        own.export_update_base64().expect("export"),
    );
    assert_refused_unchanged(&service, request);
}

#[test]
fn an_edit_made_on_the_stores_history_is_still_accepted() {
    let service = seeded();
    let state = service
        .authoring_state(&NOTE_PLAN, &note(), None)
        .expect("authoring state");
    let (mut replica, block) = client(&service, &note(), &writer(), &state);
    let mut edited = replica.materialized_document("edit").expect("document");
    edited["body"] = json!("Seeded. Edited online.");
    replica.replace_document(&edited).expect("edit");

    service
        .import(
            &NOTE_PLAN,
            request(
                &note(),
                &writer(),
                &block,
                "online-edit",
                &state.accepted_frontier_base64,
                replica
                    .export_incremental_update_base64(&state.accepted_frontier_base64)
                    .expect("export"),
            ),
            accept,
        )
        .expect("an edit on the store's history");
    assert_eq!(
        service
            .detail(&NOTE_PLAN, &note())
            .expect("detail")
            .expect("the note")["body"],
        "Seeded. Edited online."
    );
}

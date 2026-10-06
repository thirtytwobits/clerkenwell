use super::*;
use crate::testing::MemoryStorage;
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationFieldSpec,
    GeneratedCollaborationStorageKind, GeneratedCollaborationValueCodec,
};
use serde_json::json;
use std::collections::HashMap;

const fn field(
    path: &'static str,
    storage_kind: GeneratedCollaborationStorageKind,
    container: Option<&'static str>,
    key: Option<&'static str>,
    conflict: GeneratedCollaborationConflict,
) -> GeneratedCollaborationFieldSpec {
    GeneratedCollaborationFieldSpec {
        path,
        storage_kind,
        container,
        container_template: None,
        key,
        identity_path: None,
        identity_variable: None,
        order_container: None,
        item_container_template: None,
        metadata_container: None,
        metadata_container_template: None,
        metadata_key: None,
        codec: GeneratedCollaborationValueCodec::String,
        value_schema: None,
        required: true,
        required_in_parent: true,
        conflict,
        writers: &[],
    }
}

const fn plan(
    name: &'static str,
    id_field: &'static str,
    root_container: &'static str,
    fields: &'static [GeneratedCollaborationFieldSpec],
) -> GeneratedCollaborationEntitySpec {
    GeneratedCollaborationEntitySpec {
        name,
        id_field,
        schema_version: 1,
        authoring_projection: "authoringState",
        authoring_document: None,
        authoring_store_params: &[],
        import_mutation: "importUpdate",
        root_container,
        fields,
    }
}

use GeneratedCollaborationConflict::{Explicit, Immutable, Merge};
use GeneratedCollaborationStorageKind::{DerivedRevision, Scalar, Text};

/// A field holding the id of the document it is in.
const fn naming(
    path: &'static str,
    container: &'static str,
    key: &'static str,
) -> GeneratedCollaborationFieldSpec {
    GeneratedCollaborationFieldSpec {
        ..field(path, Scalar, Some(container), Some(key), Immutable)
    }
}

static NOTE_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    naming("note_id", "note", "note_id"),
    field("title", Scalar, Some("note"), Some("title"), Explicit),
    field("body", Text, Some("body"), None, Merge),
    field("summary", Text, Some("summary"), None, Merge),
    field("etag", DerivedRevision, None, None, Immutable),
];
static TASK_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    field("task_id", Scalar, Some("task"), Some("task_id"), Immutable),
    field("label", Text, Some("label"), None, Merge),
    field("etag", DerivedRevision, None, None, Immutable),
];
/// A document that names itself twice, as a card and on its face.
static CARD_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    naming("card_id", "card", "card_id"),
    naming("face.card_id", "card", "face.card_id"),
    field("label", Text, Some("label"), None, Merge),
    field("etag", DerivedRevision, None, None, Immutable),
];
static NOTE_PLAN: GeneratedCollaborationEntitySpec = plan("Note", "note_id", "note", NOTE_FIELDS);
static TASK_PLAN: GeneratedCollaborationEntitySpec = plan("Task", "task_id", "task", TASK_FIELDS);
static CARD_PLAN: GeneratedCollaborationEntitySpec = plan("Card", "card_id", "card", CARD_FIELDS);
static PLANS: &[GeneratedCollaborationEntitySpec] = &[NOTE_PLAN, TASK_PLAN, CARD_PLAN];

/// The commit policy these tests run under.
const POLICY: CommitPolicy = CommitPolicy {
    retained_operations: 8,
    attempts: match NonZeroU32::new(8) {
        Some(attempts) => attempts,
        None => panic!("attempts are non-zero"),
    },
    backoff: Duration::from_millis(2),
};

/// A validator that accepts every document.
fn accept(_: &Value) -> StoreResult<()> {
    Ok(())
}

fn note_seed() -> Value {
    json!({
        "note_id": "note-1",
        "title": "Shopping",
        "body": "Bread and milk.",
        "summary": "Groceries.",
        "etag": "",
    })
}

fn test_key() -> PeerKey {
    PeerKey::new([7; 32]).expect("a key of 32 bytes")
}

/// The principal the tests write as.
fn writer() -> Principal {
    Principal::new("writer", ActorKind::Human)
}

/// The principal the tests recover documents as.
fn operator() -> Principal {
    Principal::new("operator", ActorKind::Human)
}

fn open_service(storage: &MemoryStorage) -> CollaborationService {
    CollaborationService::new(storage.clone(), PLANS, POLICY, test_key(), "notes")
}

fn initialise(
    storage: &MemoryStorage,
) -> (
    CollaborationService,
    CollaborationDocumentId,
    Value,
    CollaborationAuthoringState,
) {
    let service = open_service(storage);
    let seed = note_seed();
    let resource_id = seed["note_id"].as_str().expect("note ID").to_string();
    let document = CollaborationDocumentId::new("Note", resource_id);
    service
        .bootstrap(&NOTE_PLAN, &document, &seed, accept)
        .expect("bootstrap collaboration document");
    let state = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .expect("read collaboration document");
    (service, document, seed, state)
}

#[test]
fn reading_missing_collaboration_documents_writes_nothing() {
    let storage = MemoryStorage::default();
    let service = open_service(&storage);
    let id = CollaborationDocumentId::new("Note", "missing");
    for _ in 0..3 {
        assert!(service.load(&id).unwrap().is_none());
        assert!(!service.verify(&id).valid);
        assert!(service.inspect(None, None, None).unwrap().0.is_empty());
        assert!(service.summaries("Note").unwrap().is_empty());
    }
    assert_eq!(storage.writes(), 0);
}

#[test]
fn concurrent_explicit_scalar_edits_block_until_rebased_resolution() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let first = edit_request(
        &document,
        &seed,
        &state,
        "explicit-first",
        &["title"],
        json!("First Writer"),
    );
    let second = edit_request(
        &document,
        &seed,
        &state,
        "explicit-second",
        &["title"],
        json!("Second Writer"),
    );

    import(&service, first, &seed).expect("first explicit edit");
    let error = import(&service, second, &seed).expect_err("explicit conflict");
    assert_eq!(error.kind, StoreErrorKind::Conflict);
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data["conflict_kind"].as_str()),
        Some("collaboration_policy")
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data["conflict_paths"].as_array())
            .and_then(|paths| paths.first())
            .and_then(Value::as_str),
        Some("title")
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .map(|data| data["base_frontier_base64"].clone()),
        Some(json!(state.accepted_frontier_base64)),
        "a policy refusal names the frontier the refused edit was based on"
    );

    let current = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .expect("fresh accepted state");
    let accepted_document = service
        .detail(&NOTE_PLAN, &document)
        .expect("current projection")
        .expect("current document");
    let resolved = edit_request(
        &document,
        &accepted_document,
        &current,
        "explicit-resolution",
        &["title"],
        json!("Resolved Writer"),
    );
    import(&service, resolved, &seed).expect("rebased explicit resolution");
    assert_eq!(
        service
            .detail(&NOTE_PLAN, &document)
            .expect("resolved projection")
            .expect("resolved document")["title"],
        json!("Resolved Writer")
    );
}

#[test]
fn an_edit_refused_under_conflict_policy_is_accepted_once_rebased_on_what_the_refusal_carries() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let first = edit_request(
        &document,
        &seed,
        &state,
        "explicit-first",
        &["title"],
        json!("First Writer"),
    );
    let second = edit_request(
        &document,
        &seed,
        &state,
        "explicit-second",
        &["title"],
        json!("Second Writer"),
    );
    import(&service, first, &seed).expect("first explicit edit");

    let refusal = import(&service, second.clone(), &seed)
        .expect_err("explicit conflict")
        .data
        .expect("refusal data");
    let accepted = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .expect("accepted state");
    assert_eq!(
        refusal["accepted_frontier_base64"],
        json!(accepted.accepted_frontier_base64),
        "a policy refusal names the accepted frontier"
    );

    // The refused client holds its base and its own edit, then takes what the refusal carries.
    let mut client = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("hydrate client");
    client
        .adopt_versioned_update_base64(state.schema_version, &second.update_base64)
        .expect("the client's own edit");
    client
        .adopt_versioned_update_base64(
            state.schema_version,
            refusal["missing_update_base64"]
                .as_str()
                .expect("the operations the client lacks"),
        )
        .expect("take the refusal's operations");
    assert!(client
        .frontier_includes(
            &client.accepted_frontier_base64(),
            &accepted.accepted_frontier_base64
        )
        .expect("the accepted frontier is known to the client"));

    let rebased = CollaborationImportRequest {
        operation_id: "explicit-rebased".to_string(),
        base_frontier_base64: accepted.accepted_frontier_base64.clone(),
        update_base64: client
            .export_incremental_update_base64(&accepted.accepted_frontier_base64)
            .expect("rebased update"),
        ..second
    };
    import(&service, rebased, &seed).expect("the rebased edit is accepted");
    assert_eq!(
        service
            .detail(&NOTE_PLAN, &document)
            .expect("accepted projection")
            .expect("accepted document")["title"],
        client
            .materialized_document("client")
            .expect("client document")["title"],
        "the accepted document is the rebased client's"
    );
}

#[test]
fn a_policy_refusal_no_concurrent_edit_caused_names_the_refused_edit_base_as_accepted() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let immutable = edit_request(
        &document,
        &seed,
        &state,
        "immutable-rewrite",
        &["note_id"],
        json!("note-2"),
    );

    let refusal = import(&service, immutable, &seed)
        .expect_err("an immutable field cannot change")
        .data
        .expect("refusal data");

    assert_eq!(refusal["conflict_kind"], json!("collaboration_policy"));
    assert_eq!(
        refusal["accepted_frontier_base64"],
        refusal["base_frontier_base64"]
    );
}

#[test]
fn an_envelope_names_the_accepted_state_it_holds_by_the_etag_readers_are_given() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let edit = edit_request(
        &document,
        &seed,
        &state,
        "etag-edit",
        &["title"],
        json!("Errands"),
    );
    let imported = import(&service, edit, &seed).expect("an edit");

    let stored = service
        .load(&document)
        .expect("read the note")
        .expect("the note");
    let read = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .expect("authoring state");

    assert_eq!(stored.etag(), imported.etag);
    assert_eq!(stored.etag(), read.etag);
    assert_ne!(
        stored.etag(),
        state.etag,
        "an edit changes the accepted state"
    );
}

fn edit_request(
    document: &CollaborationDocumentId,
    seed: &Value,
    state: &CollaborationAuthoringState,
    operation_id: &str,
    path: &[&str],
    replacement: Value,
) -> CollaborationImportRequest {
    let mut edited = seed.clone();
    let mut cursor = &mut edited;
    for segment in &path[..path.len() - 1] {
        cursor = &mut cursor[*segment];
    }
    cursor[path[path.len() - 1]] = replacement;
    let mut client = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("hydrate client");
    let block = test_key().allocate(&writer(), document);
    client.set_peer(block.base).expect("allocated peer");
    client.replace_document(&edited).expect("edit client");
    CollaborationImportRequest {
        document: document.clone(),
        schema_version: state.schema_version,
        operation_id: operation_id.to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: state.accepted_frontier_base64.clone(),
        update_base64: client
            .export_incremental_update_base64(&state.accepted_frontier_base64)
            .expect("incremental update"),
        fence: ImportFence::Frontier,
        actor: writer(),
        peer_nonces: vec![block.nonce],
        intent: None,
    }
}

fn import(
    service: &CollaborationService,
    request: CollaborationImportRequest,
    _seed: &Value,
) -> StoreResult<CollaborationImportResult> {
    service.import(&NOTE_PLAN, request, accept)
}

#[test]
fn a_peer_holding_an_accepted_frontier_receives_only_the_operations_after_it() {
    let storage = MemoryStorage::default();
    let (service, document, seed, held) = initialise(&storage);
    let later = edit_request(
        &document,
        &seed,
        &held,
        "later-title",
        &["title"],
        json!("Errands"),
    );
    import(&service, later, &seed).expect("later edit");
    let accepted = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .unwrap();
    let delivered = service
        .authoring_state(&NOTE_PLAN, &document, Some(&held.accepted_frontier_base64))
        .unwrap();
    assert_eq!(
        delivered.accepted_frontier_base64,
        accepted.accepted_frontier_base64
    );
    assert_eq!(delivered.etag, accepted.etag);
    assert!(
        !delivered.replaces_held,
        "what follows a held frontier adds to it"
    );

    let mut peer = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        held.schema_version,
        &held.update_base64,
    )
    .unwrap();
    peer.adopt_versioned_update_base64(delivered.schema_version, &delivered.update_base64)
        .expect("what follows the held frontier applies over it");
    assert_eq!(
        peer.accepted_frontier_base64(),
        accepted.accepted_frontier_base64
    );
    assert!(
        matches!(
            CollaborationReplica::from_versioned_update_base64(
                &NOTE_PLAN,
                delivered.schema_version,
                &delivered.update_base64,
            ),
            Err(CollaborationReplicaError::MissingDependency)
        ),
        "the update carries none of the operations the peer held"
    );
}

#[test]
fn a_peer_holding_a_frontier_from_another_history_receives_every_operation_in_place_of_what_it_holds(
) {
    let storage = MemoryStorage::default();
    let (service, document, _seed, _) = initialise(&storage);
    let (_, _, _, foreign) = initialise(&MemoryStorage::default());
    let accepted = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .unwrap();
    let delivered = service
        .authoring_state(
            &NOTE_PLAN,
            &document,
            Some(&foreign.accepted_frontier_base64),
        )
        .unwrap();
    let peer = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        delivered.schema_version,
        &delivered.update_base64,
    )
    .expect("the update stands alone");
    assert_eq!(
        peer.accepted_frontier_base64(),
        accepted.accepted_frontier_base64
    );
    assert!(
        delivered.replaces_held,
        "the peer replaces what it holds with the state delivered"
    );
    assert!(
        !accepted.replaces_held,
        "a peer that holds nothing has nothing replaced"
    );
}

#[test]
fn captured_text_consumption_preserves_later_edits_across_restart_and_retry() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let captured = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .unwrap();
    let path = "body";
    let text = captured
        .text_at_frontier(path, &HashMap::new(), &state.accepted_frontier_base64)
        .unwrap();
    // Each preparation writes under its own peer of the writer's block.
    let block = service.allocate_peers(&writer(), &document);
    let prepared = captured
        .prepare_text_replacement_at_frontier(
            path,
            &HashMap::new(),
            &state.accepted_frontier_base64,
            &text,
            "",
            block.base | 1,
        )
        .unwrap();
    // A replacement after capture deletes the old characters and inserts new ones.
    // Consumption must target the old IDs even when their text is already gone.
    let later = "Keep the next draft 🦊";
    let replacement = captured
        .prepare_text_replacement_at_frontier(
            path,
            &HashMap::new(),
            &state.accepted_frontier_base64,
            &text,
            later,
            block.base | 2,
        )
        .unwrap();
    let prepared_request = |operation_id: &str, update_base64: String| CollaborationImportRequest {
        document: document.clone(),
        schema_version: state.schema_version,
        operation_id: operation_id.to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: state.accepted_frontier_base64.clone(),
        update_base64,
        fence: ImportFence::Frontier,
        actor: writer(),
        peer_nonces: vec![block.nonce.clone()],
        intent: None,
    };
    import(&service, prepared_request("later-text", replacement), &seed).unwrap();
    let request = prepared_request("captured-consumption", prepared);
    let before = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .unwrap();
    // Rejection must leave accepted text and the durable frontier untouched.
    assert!(service
        .import(&NOTE_PLAN, request.clone(), |_| {
            Err(StoreError::invalid_request("Rejected test acceptance"))
        })
        .is_err());
    assert_eq!(
        service
            .authoring_state(&NOTE_PLAN, &document, None)
            .unwrap()
            .accepted_frontier_base64,
        before.accepted_frontier_base64
    );
    import(&service, request.clone(), &seed).unwrap();
    drop(service);
    let restarted = open_service(&storage);
    let retry = import(&restarted, request, &seed).unwrap();
    assert!(retry.duplicate);
    let actual = restarted.detail(&NOTE_PLAN, &document).unwrap().unwrap();
    assert_eq!(actual["body"], later);
    let mut expected = seed;
    expected["body"] = later.into();
    expected["etag"] = actual["etag"].clone();
    assert_eq!(actual, expected);
}

#[test]
fn pre_commit_faults_leave_the_previous_generation_atomically_visible() {
    for point in [
        CollaborationFaultPoint::CandidateImported,
        CollaborationFaultPoint::Materialised,
        CollaborationFaultPoint::Validated,
    ] {
        let storage = MemoryStorage::default();
        let (service, document, seed, state) = initialise(&storage);
        let before = service
            .load(&document)
            .expect("read baseline")
            .expect("baseline envelope");
        let request = edit_request(
            &document,
            &seed,
            &state,
            "pre-commit-fault",
            &["body"],
            json!("retained local edit"),
        );
        service.faults.inject(point);

        let error = import(&service, request, &seed).expect_err("fault must interrupt import");
        assert_eq!(
            error.data.as_ref().and_then(|data| data["code"].as_str()),
            Some("collaboration_fault_injected")
        );
        let after = service
            .load(&document)
            .expect("read after fault")
            .expect("baseline remains");
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.checkpoint_sha256, before.checkpoint_sha256);
    }
}

#[test]
fn a_failed_port_write_leaves_the_previous_generation_visible() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let before = service
        .load(&document)
        .expect("read baseline")
        .expect("baseline envelope");
    let request = edit_request(
        &document,
        &seed,
        &state,
        "failed-write",
        &["body"],
        json!("retained local edit"),
    );
    storage.fail_swaps(true);

    import(&service, request, &seed).expect_err("the failed write interrupts the import");
    storage.fail_swaps(false);
    for reader in [&service, &open_service(&storage)] {
        let after = reader
            .load(&document)
            .expect("read after the failed write")
            .expect("baseline remains");
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.checkpoint_sha256, before.checkpoint_sha256);
    }
}

#[test]
fn a_fault_after_the_commit_leaves_the_new_generation_accepted() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let before = service
        .load(&document)
        .expect("read baseline")
        .expect("baseline envelope");
    let replacement = json!("durably accepted edit");
    let request = edit_request(
        &document,
        &seed,
        &state,
        "post-commit-fault",
        &["body"],
        replacement.clone(),
    );
    service
        .faults
        .inject(CollaborationFaultPoint::AfterDurableCommit);

    import(&service, request, &seed).expect_err("fault reports the interrupted commit");
    let after = open_service(&storage)
        .load(&document)
        .expect("restart reads committed generation")
        .expect("committed envelope");
    assert!(after.generation > before.generation);
    let materialized = service
        .detail(&NOTE_PLAN, &document)
        .expect("materialize accepted generation")
        .expect("accepted detail");
    assert_eq!(materialized["body"], replacement);
}

#[test]
fn two_services_over_one_store_serialise_concurrent_commits_and_converge() {
    let storage = MemoryStorage::default();
    let (writer_a, document, seed, state) = initialise(&storage);
    let writer_b = open_service(&storage);
    let request_a = edit_request(
        &document,
        &seed,
        &state,
        "writer-a",
        &["body"],
        json!("edit from writer A"),
    );
    let replay_a = request_a.clone();
    let request_b = edit_request(
        &document,
        &seed,
        &state,
        "writer-b",
        &["summary"],
        json!("edit from writer B"),
    );
    let seed_a = seed.clone();
    let seed_b = seed.clone();
    let thread_a = std::thread::spawn(move || import(&writer_a, request_a, &seed_a));
    let writer_b_import = writer_b.clone();
    let thread_b = std::thread::spawn(move || import(&writer_b_import, request_b, &seed_b));
    let accepted_a = thread_a.join().expect("writer A thread").expect("writer A");
    let accepted_b = thread_b.join().expect("writer B thread").expect("writer B");

    assert_ne!(accepted_a.generation, accepted_b.generation);
    let converged_a = open_service(&storage)
        .detail(&NOTE_PLAN, &document)
        .expect("writer A projection")
        .expect("writer A detail");
    let converged_b = writer_b
        .detail(&NOTE_PLAN, &document)
        .expect("writer B projection")
        .expect("writer B detail");
    assert_eq!(converged_a, converged_b);
    assert_eq!(converged_a["body"], json!("edit from writer A"));
    assert_eq!(converged_a["summary"], json!("edit from writer B"));

    let duplicate = import(&writer_b, replay_a, &seed).expect("a retry through the other service");
    assert!(duplicate.duplicate);
    let envelope = writer_b
        .load(&document)
        .expect("retained window")
        .expect("envelope");
    assert!(envelope.retained_operations.len() >= 3);
}

#[test]
fn corruption_is_reported_and_checkpoint_repair_preserves_evidence() {
    let storage = MemoryStorage::default();
    let (service, document, _seed, _) = initialise(&storage);
    let original = storage
        .read(&storage.source(&document))
        .expect("read envelope")
        .expect("original envelope");
    let mut envelope: DurableCollaborationEnvelope =
        serde_json::from_slice(&original.bytes).expect("parse envelope");
    envelope.retained_operations[0].update_sha256 = "corrupt".to_string();
    let corrupt_bytes = serde_json::to_vec_pretty(&envelope).expect("encode corrupt envelope");
    storage
        .compare_and_swap(&document, Some(&original.version), &corrupt_bytes)
        .expect("install corruption")
        .expect("the envelope is unchanged");

    let inspection = service.verify(&document);
    assert!(!inspection.valid);
    assert!(service.detail(&NOTE_PLAN, &document).is_err());
    service
        .repair(&operator(), &document, "checksum verification failed")
        .expect("repair from valid checkpoint");
    assert_eq!(storage.evidence(), vec![corrupt_bytes]);
    assert!(service.verify(&document).valid);
    assert!(service
        .detail(&NOTE_PLAN, &document)
        .expect("materialize repaired checkpoint")
        .is_some());
}

#[test]
fn operator_recovery_requests_are_durably_audited_without_reason_text() {
    let storage = MemoryStorage::default();
    let (service, document, _, _) = initialise(&storage);
    let private_reason = "operator supplied private incident context";

    service
        .export_for_recovery(&operator(), &document)
        .expect("audited export");
    service
        .quarantine(&operator(), &document, private_reason)
        .expect("audited quarantine");
    service.reindex(&operator()).expect("audited reindex");
    service
        .reset(&operator(), &document)
        .expect("audited destructive reset");

    let records = service
        .audit(&CollaborationAuditQuery::default())
        .expect("read the audit");
    let recoveries: Vec<_> = records
        .iter()
        .map(|record| match &record.event {
            CollaborationAuditEvent::Recovery {
                action,
                destructive,
                ..
            } => (*action, *destructive),
            event => panic!("only recovery was requested, not {event:?}"),
        })
        .collect();
    assert_eq!(
        recoveries,
        vec![
            (CollaborationRecoveryAction::Export, false),
            (CollaborationRecoveryAction::Quarantine, false),
            (CollaborationRecoveryAction::Reindex, false),
            (CollaborationRecoveryAction::Reset, true),
        ]
    );
    assert!(records.iter().all(|record| record.actor == operator()));
    assert!(!serde_json::to_string(&records)
        .expect("audit JSON")
        .contains(private_reason));
}

#[test]
fn a_commit_adding_operations_under_a_peer_bound_to_another_principal_is_refused() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let edit = edit_request(
        &document,
        &seed,
        &state,
        "bound",
        &["title"],
        json!("Bound"),
    );
    import(&service, edit, &seed).expect("the writer's edit");
    let resident = service
        .read_envelope(&document)
        .expect("read")
        .expect("the note");
    let bound: Vec<u64> = resident
        .envelope
        .peers
        .iter()
        .filter(|(_, principal)| **principal == writer())
        .map(|(peer, _)| *peer)
        .collect();
    assert!(!bound.is_empty());

    let refused = service
        .commit(
            Some(&resident),
            CollaborationCommit {
                document: document.clone(),
                schema_version: NOTE_PLAN.schema_version,
                operation_id: "impostor".to_string(),
                actor: Principal::new("impostor", ActorKind::Human),
                intent: None,
                peers: bound,
                imported_update: b"impostor".to_vec(),
                has_new_operations: true,
                accepted_update: b"impostor".to_vec(),
                frontier_before: None,
                frontier_after: None,
            },
        )
        .expect_err("the peers are the writer's");

    assert_eq!(
        refused.data.as_ref().and_then(|data| data["code"].as_str()),
        Some("collaboration_peer_bound")
    );
    assert_eq!(
        service
            .load(&document)
            .expect("read")
            .expect("the note")
            .generation,
        resident.envelope.generation
    );
}

/// A structurally valid envelope whose checkpoint holds no operations:
/// enough for anything that reads envelopes without materialising them.
fn opaque_envelope(
    entity: &str,
    resource_id: &str,
    schema_version: u32,
) -> DurableCollaborationEnvelope {
    let checkpoint = &loro::LoroDoc::new()
        .export(loro::ExportMode::all_updates())
        .expect("an empty document");
    DurableCollaborationEnvelope {
        envelope_version: ENVELOPE_VERSION,
        entity: entity.to_string(),
        resource_id: resource_id.to_string(),
        schema_version,
        generation: 1,
        checkpoint_sequence: 0,
        compacted_through_sequence: 0,
        checkpoint_update_base64: BASE64.encode(checkpoint),
        checkpoint_sha256: sha256_hex(checkpoint),
        checkpoint_bytes: checkpoint.len(),
        retained_operations: Vec::new(),
        peers: BTreeMap::new(),
    }
}

#[test]
fn reads_leave_every_stored_byte_unchanged() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let request = edit_request(
        &document,
        &seed,
        &state,
        "read-purity-edit",
        &["body"],
        json!("an accepted edit"),
    );
    import(&service, request, &seed).expect("accepted edit");
    let before = (storage.envelopes(), storage.writes());

    for _ in 0..2 {
        service.detail(&NOTE_PLAN, &document).expect("detail");
        service
            .authoring_state(&NOTE_PLAN, &document, None)
            .expect("authoring state");
        service.load(&document).expect("load");
        service.summary(&document).expect("summary");
        service.summaries("Note").expect("summaries");
        service
            .read_document(&NOTE_PLAN, &document)
            .expect("read document");
        service.inspect(None, None, None).expect("inspect");
        service.verify(&document);
        service
            .audit(&CollaborationAuditQuery::default())
            .expect("audit");
        service.attribution(&document).expect("attribution");
        service.allocate_peers(&writer(), &document);
        service.counters();
    }

    assert_eq!((storage.envelopes(), storage.writes()), before);
}

#[test]
fn reindex_reads_every_document_and_changes_nothing_but_the_audit() {
    let storage = MemoryStorage::default();
    let service = open_service(&storage);
    let count = 250;
    for index in 0..count {
        service
            .install(opaque_envelope(
                PLANS[index % PLANS.len()].name,
                &format!("document-{index:03}"),
                1,
            ))
            .expect("install envelope");
    }
    let before = storage.envelopes();
    let audit = || {
        storage
            .audit(&CollaborationAuditQuery::default())
            .expect("audit")
            .len()
    };
    let audited = audit();

    let (documents, revision) = service.reindex(&operator()).expect("reindex");
    let (again, same_revision) = service.reindex(&operator()).expect("reindex again");

    assert_eq!(documents, count);
    assert_eq!(again, count);
    assert_eq!(
        revision, same_revision,
        "an unchanged catalogue keeps its fingerprint"
    );
    assert_eq!(storage.envelopes(), before);
    assert_eq!(audit(), audited + 2);
}

#[test]
fn inspection_reports_a_required_migration_for_every_generated_plan() {
    for spec in PLANS {
        let envelope = |schema_version| opaque_envelope(spec.name, "resource", schema_version);
        assert!(
            !inspection_from_envelope(PLANS, envelope(spec.schema_version), true, None)
                .migration_required,
            "{} at its current schema version needs no migration",
            spec.name
        );
        assert!(
            inspection_from_envelope(PLANS, envelope(spec.schema_version + 1), true, None)
                .migration_required,
            "{} at another schema version needs a migration",
            spec.name
        );
    }
}

#[test]
fn diagnostic_inspection_is_bounded_and_contains_no_document_payload() {
    let storage = MemoryStorage::default();
    let (service, document, _, _) = initialise(&storage);
    let (inspections, truncated) = service
        .inspect(Some("Note"), Some(&document.resource_id), Some(1))
        .expect("targeted inspection");
    assert!(!truncated);
    assert_eq!(inspections.len(), 1);
    let serialized = serde_json::to_string(&inspections).expect("diagnostic JSON");
    assert!(!serialized.contains("update_base64"));
    assert!(!serialized.contains(&note_seed()["body"].as_str().unwrap().to_string()));
    assert!(!serialized.contains("checkpoint_update"));
}

fn on_read(
    mut request: CollaborationImportRequest,
    read: &CollaborationAuthoringState,
) -> CollaborationImportRequest {
    request.fence = ImportFence::Etag(read.etag.clone());
    request
}

#[test]
fn an_etag_fenced_import_of_the_current_read_commits() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let replacement = json!("committed on the read it was made from");
    let request = on_read(
        edit_request(
            &document,
            &seed,
            &state,
            "current-read",
            &["body"],
            replacement.clone(),
        ),
        &state,
    );

    import(&service, request, &seed).expect("the read is current");

    assert_eq!(
        service.detail(&NOTE_PLAN, &document).unwrap().unwrap()["body"],
        replacement
    );
}

#[test]
fn an_etag_fenced_import_of_a_superseded_read_is_refused_and_changes_nothing() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let first = edit_request(
        &document,
        &seed,
        &state,
        "first",
        &["summary"],
        json!("first"),
    );
    import(&service, first, &seed).expect("an earlier writer commits");
    let before = service.load(&document).unwrap().unwrap();
    let stale = on_read(
        edit_request(&document, &seed, &state, "stale", &["body"], json!("stale")),
        &state,
    );

    let error = import(&service, stale, &seed).expect_err("the read was superseded");

    assert_eq!(error.kind, StoreErrorKind::Conflict);
    let data = error.data.expect("the refusal carries data");
    let accepted = service
        .authoring_state(&NOTE_PLAN, &document, None)
        .unwrap();
    assert_eq!(data["conflict_kind"], "collaboration_revision");
    assert_eq!(data["expected_etag"], json!(state.etag));
    assert_eq!(data["current_etag"], json!(accepted.etag));
    assert_eq!(data[NOTE_PLAN.id_field], json!(document.resource_id));
    assert_eq!(data["current"]["summary"], json!("first"));
    let after = service.load(&document).unwrap().unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.checkpoint_sha256, before.checkpoint_sha256);
}

#[test]
fn a_retried_etag_fenced_import_is_a_duplicate_not_a_stale_read() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let request = on_read(
        edit_request(
            &document,
            &seed,
            &state,
            "retried",
            &["body"],
            json!("once"),
        ),
        &state,
    );
    let accepted = import(&service, request.clone(), &seed).expect("first delivery");

    let retried = import(&service, request, &seed).expect("the retry is recognised");

    assert!(retried.duplicate);
    assert_eq!(retried.generation, accepted.generation);
}

#[test]
fn etag_fenced_imports_racing_from_one_read_through_two_services_commit_exactly_once() {
    let storage = MemoryStorage::default();
    let (writer_a, document, seed, state) = initialise(&storage);
    let writer_b = open_service(&storage);
    let request_a = on_read(
        edit_request(
            &document,
            &seed,
            &state,
            "race-a",
            &["body"],
            json!("from writer A"),
        ),
        &state,
    );
    let request_b = on_read(
        edit_request(
            &document,
            &seed,
            &state,
            "race-b",
            &["summary"],
            json!("from writer B"),
        ),
        &state,
    );
    let (seed_a, seed_b) = (seed.clone(), seed.clone());
    let thread_a = std::thread::spawn(move || import(&writer_a, request_a, &seed_a));
    let thread_b = std::thread::spawn(move || import(&writer_b, request_b, &seed_b));
    let outcomes = [
        thread_a.join().expect("writer A thread"),
        thread_b.join().expect("writer B thread"),
    ];

    let committed = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert_eq!(
        committed, 1,
        "exactly one writer of the shared read commits"
    );
    let refused = outcomes
        .iter()
        .find_map(|outcome| outcome.as_ref().err())
        .expect("the other is refused");
    assert_eq!(
        refused
            .data
            .as_ref()
            .map(|data| data["conflict_kind"].clone()),
        Some(json!("collaboration_revision"))
    );
}

/// A writer's replica holding `state`, edited at `path`, and its import of that edit.
fn writer_edit(
    document: &CollaborationDocumentId,
    seed: &Value,
    state: &CollaborationAuthoringState,
    operation_id: &str,
    path: &str,
    replacement: Value,
) -> (CollaborationReplica, CollaborationImportRequest) {
    let mut replica = CollaborationReplica::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("hydrate writer");
    let block = test_key().allocate(&writer(), document);
    replica.set_peer(block.base).expect("allocated peer");
    let mut edited = seed.clone();
    edited[path] = replacement;
    replica.replace_document(&edited).expect("edit writer");
    let request = CollaborationImportRequest {
        document: document.clone(),
        schema_version: state.schema_version,
        operation_id: operation_id.to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: state.accepted_frontier_base64.clone(),
        update_base64: replica
            .export_incremental_update_base64(&state.accepted_frontier_base64)
            .expect("incremental update"),
        fence: ImportFence::Frontier,
        actor: writer(),
        peer_nonces: vec![block.nonce],
        intent: None,
    };
    (replica, request)
}

fn update_metadata(update_base64: &str) -> loro::ImportBlobMetadata {
    loro::LoroDoc::decode_import_blob_meta(&BASE64.decode(update_base64).unwrap(), false).unwrap()
}

#[test]
fn an_import_reply_carries_only_the_operations_the_importer_lacks() {
    let storage = MemoryStorage::default();
    let (service, document, seed, state) = initialise(&storage);
    let (_, first_request) = writer_edit(
        &document,
        &seed,
        &state,
        "first",
        "body",
        json!("Bread, milk and eggs."),
    );
    let (mut second, second_request) = writer_edit(
        &document,
        &seed,
        &state,
        "second",
        "summary",
        json!("The weekly shop."),
    );

    let first_reply = import(&service, first_request, &seed).expect("first import");
    assert_eq!(
        update_metadata(&first_reply.missing_update_base64).change_num,
        0,
        "The first writer holds everything the store accepted"
    );

    let own = update_metadata(&second_request.update_base64).partial_end_vv;
    for reply in [
        import(&service, second_request.clone(), &seed).expect("second import"),
        import(&service, second_request, &seed).expect("retried second import"),
    ] {
        let missing = update_metadata(&reply.missing_update_base64);
        assert!(
            missing.change_num > 0,
            "The second writer lacks the first edit"
        );
        assert!(
            own.iter()
                .all(|(peer, _)| missing.partial_end_vv.get(peer).is_none()),
            "A reply carries none of the importer's own operations"
        );
        second
            .adopt_versioned_update_base64(reply.schema_version, &reply.missing_update_base64)
            .expect("take the reply");
        assert_eq!(
            second.materialized_document(&reply.etag).unwrap(),
            reply.materialized,
            "The reply brings the importer to the accepted document"
        );
    }
}

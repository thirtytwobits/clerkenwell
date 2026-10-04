//! The collaboration service commits through any storage port that keeps
//! nothing but bytes and versions.

mod support;

use clerkenwell_doc::CollaborationReplica;
use clerkenwell_store::testing::{self, DurableCollaborationEnvelope, MemoryStorage};
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationRecoveryAction, CollaborationService, CollaborationStoragePort, CommitPolicy,
    ImportFence,
};
use serde_json::{json, Value};
use std::num::NonZeroU32;
use support::{accept, Renamed, NOTE_PLAN, PLANS, POLICY};

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

fn seed_document() -> Value {
    json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" })
}

/// One client's history of a note: its seed, then each edit as an operation
/// id, the frontier it was made on and the operations it made.
struct Edits {
    seed_update: String,
    edits: Vec<(String, String, String)>,
}

fn edits(count: usize) -> Edits {
    let mut client =
        CollaborationReplica::from_document(&NOTE_PLAN, &seed_document()).expect("seed client");
    let seed_update = client.export_update_base64().expect("seed update");
    let mut edits = Vec::new();
    for index in 0..count {
        let base = client.accepted_frontier_base64();
        let mut edited = seed_document();
        edited["body"] = Value::from(format!("Edit {index}."));
        client.replace_document(&edited).expect("edit client");
        let update = client
            .export_incremental_update_base64(&base)
            .expect("incremental update");
        edits.push((format!("edit-{index}"), base, update));
    }
    Edits { seed_update, edits }
}

fn request(operation_id: &str, base: &str, update: &str) -> CollaborationImportRequest {
    CollaborationImportRequest {
        document: note(),
        schema_version: NOTE_PLAN.schema_version,
        operation_id: operation_id.to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: base.to_string(),
        update_base64: update.to_string(),
        fence: ImportFence::Frontier,
    }
}

/// What an envelope says about a document's history, leaving out how its
/// checkpoint is encoded.
fn history(envelope: &DurableCollaborationEnvelope) -> Value {
    json!({
        "resource_id": envelope.resource_id,
        "schema_version": envelope.schema_version,
        "generation": envelope.generation,
        "checkpoint_sequence": envelope.checkpoint_sequence,
        "compacted_through_sequence": envelope.compacted_through_sequence,
        "retained": envelope
            .retained_operations
            .iter()
            .map(|operation| json!([operation.operation_id, operation.sequence]))
            .collect::<Vec<_>>(),
    })
}

/// Drives one document through commits, a retry and recovery, recording what
/// the service reports.
fn drive(service: &CollaborationService, edits: &Edits) -> Value {
    let mut observed = Vec::new();
    let document = note();
    service
        .bootstrap_update(
            &NOTE_PLAN,
            &document,
            NOTE_PLAN.schema_version,
            &edits.seed_update,
            accept,
        )
        .expect("seed");
    for (operation_id, base, update) in &edits.edits {
        let imported = service
            .import(&NOTE_PLAN, request(operation_id, base, update), accept)
            .expect("commit");
        assert!(!imported.duplicate, "{operation_id} is new");
    }
    let (operation_id, base, update) = edits.edits.last().expect("an edit");
    let retried = service
        .import(&NOTE_PLAN, request(operation_id, base, update), accept)
        .expect("retry");
    assert!(retried.duplicate, "a retried operation is a duplicate");
    observed.push(history(
        &testing::load(service, &document).unwrap().unwrap(),
    ));

    observed.push(service.detail(&NOTE_PLAN, &document).unwrap().unwrap());

    service
        .repair(&document, "drop the window")
        .expect("repair");
    observed.push(history(
        &testing::load(service, &document).unwrap().unwrap(),
    ));
    assert!(service.verify(&document).valid);
    let exported = service.export_for_recovery(&document).expect("export");
    assert!(!exported.is_empty());
    service
        .quarantine(&document, "evidence")
        .expect("quarantine");
    service.reset(&document).expect("reset");
    assert!(service.summary(&document).unwrap().is_none());
    observed.push(json!(service
        .recovery_audit()
        .unwrap()
        .iter()
        .map(|record| record.action)
        .collect::<Vec<_>>()));
    Value::Array(observed)
}

#[test]
fn ports_naming_sources_and_versions_differently_carry_the_protocol_alike() {
    let edits = edits(12);
    let renamed = drive(
        &CollaborationService::new(Renamed::default(), PLANS, POLICY, "renamed"),
        &edits,
    );
    let in_memory = drive(
        &CollaborationService::new(MemoryStorage::default(), PLANS, POLICY, "memory"),
        &edits,
    );

    assert_eq!(in_memory, renamed);
    let audit = in_memory
        .as_array()
        .and_then(|observed| observed.last())
        .cloned();
    assert_eq!(
        audit,
        Some(json!([
            CollaborationRecoveryAction::Repair,
            CollaborationRecoveryAction::Export,
            CollaborationRecoveryAction::Quarantine,
            CollaborationRecoveryAction::Reset,
        ]))
    );
}

#[test]
fn quarantine_preserves_the_stored_bytes_through_the_port() {
    let port = MemoryStorage::default();
    let service = CollaborationService::new(port.clone(), PLANS, POLICY, "memory");
    let document = note();
    service
        .bootstrap(&NOTE_PLAN, &document, &seed_document(), accept)
        .expect("seed");

    service
        .quarantine(&document, "evidence")
        .expect("quarantine");

    let stored = port
        .read(&port.source(&document))
        .expect("read")
        .expect("stored");
    assert_eq!(port.evidence(), vec![stored.bytes]);
}

#[test]
fn commits_racing_through_one_port_both_land() {
    let port = MemoryStorage::default();
    let first = CollaborationService::new(port.clone(), PLANS, POLICY, "memory");
    let second = CollaborationService::new(port, PLANS, POLICY, "memory");
    let document = note();
    first
        .bootstrap(&NOTE_PLAN, &document, &seed_document(), accept)
        .expect("seed");
    let state = first
        .authoring_state(&NOTE_PLAN, &document, None)
        .expect("read");
    let edit = |body: &str| {
        let mut client = CollaborationReplica::from_versioned_update_base64(
            &NOTE_PLAN,
            state.schema_version,
            &state.update_base64,
        )
        .expect("client");
        let mut edited = seed_document();
        edited["body"] = Value::from(body);
        client.replace_document(&edited).expect("edit");
        client
            .export_incremental_update_base64(&state.accepted_frontier_base64)
            .expect("update")
    };
    let (update_a, update_b) = (edit("From A."), edit("From B."));
    let base_a = state.accepted_frontier_base64.clone();
    let base_b = base_a.clone();

    let thread_a = std::thread::spawn(move || {
        first.import(&NOTE_PLAN, request("race-a", &base_a, &update_a), accept)
    });
    let thread_b = std::thread::spawn(move || {
        second.import(&NOTE_PLAN, request("race-b", &base_b, &update_b), accept)
    });
    let accepted_a = thread_a.join().expect("thread A").expect("A commits");
    let accepted_b = thread_b.join().expect("thread B").expect("B commits");

    assert_ne!(accepted_a.generation, accepted_b.generation);
}

#[test]
fn an_import_that_keeps_losing_its_commit_is_refused_after_the_attempts_its_policy_allows() {
    let edits = edits(1);
    let port = MemoryStorage::default();
    let policy = CommitPolicy {
        attempts: NonZeroU32::new(3).expect("attempts"),
        ..POLICY
    };
    let service = CollaborationService::new(port.clone(), PLANS, policy, "memory");
    service
        .bootstrap_update(
            &NOTE_PLAN,
            &note(),
            NOTE_PLAN.schema_version,
            &edits.seed_update,
            accept,
        )
        .expect("seed");
    port.lose_swaps(true);
    let writes = port.writes();

    let (operation_id, base, update) = &edits.edits[0];
    let refused = service
        .import(&NOTE_PLAN, request(operation_id, base, update), accept)
        .expect_err("every commit loses");

    assert_eq!(
        refused.data.as_ref().and_then(|data| data["code"].as_str()),
        Some("collaboration_commit_contended")
    );
    assert_eq!(port.writes() - writes, policy.attempts.get() as usize);
}

#[test]
fn an_envelope_keeps_the_latest_operations_its_policy_retains() {
    let edits = edits(6);
    let policy = CommitPolicy {
        retained_operations: 3,
        ..POLICY
    };
    let service = CollaborationService::new(MemoryStorage::default(), PLANS, policy, "memory");
    service
        .bootstrap_update(
            &NOTE_PLAN,
            &note(),
            NOTE_PLAN.schema_version,
            &edits.seed_update,
            accept,
        )
        .expect("seed");
    for (operation_id, base, update) in &edits.edits {
        service
            .import(&NOTE_PLAN, request(operation_id, base, update), accept)
            .expect("commit");
    }

    let kept = testing::load(&service, &note())
        .unwrap()
        .expect("the note")
        .retained_operations
        .into_iter()
        .map(|operation| operation.operation_id)
        .collect::<Vec<_>>();
    let latest = edits.edits[edits.edits.len() - policy.retained_operations..]
        .iter()
        .map(|(operation_id, _, _)| operation_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(kept, latest);
}

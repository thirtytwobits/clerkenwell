//! The collaboration service commits through any storage port that keeps
//! nothing but bytes and versions.

mod support;

use clerkenwell_doc::CollaborationReplica;
use clerkenwell_store::testing::{self, DurableCollaborationEnvelope, MemoryStorage};
use clerkenwell_store::{
    CollaborationAuditEvent, CollaborationAuditQuery, CollaborationDocumentId,
    CollaborationImportRequest, CollaborationService, CollaborationStoragePort, CommitPolicy,
    PeerBlock,
};
use serde_json::{json, Value};
use std::num::NonZeroU32;
use support::{accept, key, operator, writer, Renamed, NOTE_PLAN, PLANS, POLICY};

/// The peer the seed is written under, outside every block the tests allocate.
const SEED_PEER: u64 = 1;

fn note() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Note", "note-1")
}

fn seed_document() -> Value {
    json!({ "note_id": "note-1", "body": "Seeded.", "etag": "" })
}

/// One client's history of a note: its seed, then each edit as an operation
/// id, the frontier it was made on and the operations it made under `block`.
struct Edits {
    seed_update: String,
    block: PeerBlock,
    edits: Vec<(String, String, String)>,
}

/// A history of `count` edits the writer makes under a block a service
/// holding the tests' key allocated.
fn edits(count: usize) -> Edits {
    let block = CollaborationService::new(MemoryStorage::default(), PLANS, POLICY, key(), "any")
        .allocate_peers(&writer(), &note());
    let mut client = CollaborationReplica::from_document(&NOTE_PLAN, &seed_document(), SEED_PEER)
        .expect("seed client");
    let seed_update = client.export_update_base64().expect("seed update");
    client.set_peer(block.base).expect("allocated peer");
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
    Edits {
        seed_update,
        block,
        edits,
    }
}

fn request(
    edits: &Edits,
    operation_id: &str,
    base: &str,
    update: &str,
) -> CollaborationImportRequest {
    support::request(
        &note(),
        &writer(),
        &edits.block,
        operation_id,
        base,
        update.to_string(),
    )
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
            .import(
                &NOTE_PLAN,
                request(edits, operation_id, base, update),
                accept,
            )
            .expect("commit");
        assert!(!imported.duplicate, "{operation_id} is new");
    }
    let (operation_id, base, update) = edits.edits.last().expect("an edit");
    let retried = service
        .import(
            &NOTE_PLAN,
            request(edits, operation_id, base, update),
            accept,
        )
        .expect("retry");
    assert!(retried.duplicate, "a retried operation is a duplicate");
    observed.push(history(
        &testing::load(service, &document).unwrap().unwrap(),
    ));

    observed.push(service.detail(&NOTE_PLAN, &document).unwrap().unwrap());

    service
        .repair(&operator(), &document, "drop the window")
        .expect("repair");
    observed.push(history(
        &testing::load(service, &document).unwrap().unwrap(),
    ));
    assert!(service.verify(&document).valid);
    let exported = service
        .export_for_recovery(&operator(), &document)
        .expect("export");
    assert!(!exported.is_empty());
    service
        .quarantine(&operator(), &document, "evidence")
        .expect("quarantine");
    service.reset(&operator(), &document).expect("reset");
    assert!(service.summary(&document).unwrap().is_none());
    observed.push(json!(service
        .audit(&CollaborationAuditQuery::default())
        .unwrap()
        .iter()
        .map(|record| match &record.event {
            CollaborationAuditEvent::Recovery { action, .. } => json!(action),
            CollaborationAuditEvent::Refusal { code, .. } => json!(code),
        })
        .collect::<Vec<_>>()));
    Value::Array(observed)
}

#[test]
fn ports_naming_sources_and_versions_differently_carry_the_protocol_alike() {
    let edits = edits(12);
    let renamed = drive(
        &CollaborationService::new(Renamed::default(), PLANS, POLICY, key(), "renamed"),
        &edits,
    );
    let in_memory = drive(
        &CollaborationService::new(MemoryStorage::default(), PLANS, POLICY, key(), "memory"),
        &edits,
    );

    assert_eq!(in_memory, renamed);
    let audit = in_memory
        .as_array()
        .and_then(|observed| observed.last())
        .cloned();
    assert_eq!(
        audit,
        Some(json!(["repair", "export", "quarantine", "reset"]))
    );
}

#[test]
fn quarantine_preserves_the_stored_bytes_through_the_port() {
    let port = MemoryStorage::default();
    let service = CollaborationService::new(port.clone(), PLANS, POLICY, key(), "memory");
    let document = note();
    service
        .bootstrap(&NOTE_PLAN, &document, &seed_document(), accept)
        .expect("seed");

    service
        .quarantine(&operator(), &document, "evidence")
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
    let first = CollaborationService::new(port.clone(), PLANS, POLICY, key(), "memory");
    let second = CollaborationService::new(port, PLANS, POLICY, key(), "memory");
    let document = note();
    first
        .bootstrap(&NOTE_PLAN, &document, &seed_document(), accept)
        .expect("seed");
    let state = first
        .authoring_state(&NOTE_PLAN, &document, None)
        .expect("read");
    let edit = |operation_id: &str, body: &str| {
        let (mut client, block) = support::client(&first, &document, &writer(), &state);
        let mut edited = seed_document();
        edited["body"] = Value::from(body);
        client.replace_document(&edited).expect("edit");
        support::request(
            &document,
            &writer(),
            &block,
            operation_id,
            &state.accepted_frontier_base64,
            client
                .export_incremental_update_base64(&state.accepted_frontier_base64)
                .expect("update"),
        )
    };
    let (request_a, request_b) = (edit("race-a", "From A."), edit("race-b", "From B."));

    let thread_a = std::thread::spawn(move || first.import(&NOTE_PLAN, request_a, accept));
    let thread_b = std::thread::spawn(move || second.import(&NOTE_PLAN, request_b, accept));
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
    let service = CollaborationService::new(port.clone(), PLANS, policy, key(), "memory");
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
        .import(
            &NOTE_PLAN,
            request(&edits, operation_id, base, update),
            accept,
        )
        .expect_err("every commit loses");

    assert_eq!(
        refused.data.as_ref().and_then(|data| data["code"].as_str()),
        Some("collaboration_commit_contended")
    );
    // Each attempt's swap, then the refusal's audit record.
    assert_eq!(port.writes() - writes, policy.attempts.get() as usize + 1);
}

#[test]
fn an_envelope_keeps_the_latest_operations_its_policy_retains() {
    let edits = edits(6);
    let policy = CommitPolicy {
        retained_operations: 3,
        ..POLICY
    };
    let service =
        CollaborationService::new(MemoryStorage::default(), PLANS, policy, key(), "memory");
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
            .import(
                &NOTE_PLAN,
                request(&edits, operation_id, base, update),
                accept,
            )
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

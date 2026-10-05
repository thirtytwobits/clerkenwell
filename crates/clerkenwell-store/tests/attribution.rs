//! Every accepted change names the principal that made it, and a writer's
//! operations are accepted only under peers allocated to it.

mod support;

use std::time::{Duration, SystemTime};

use clerkenwell_doc::CollaborationReplica;
use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind, GeneratedWriterConflict,
};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationAuditEvent, CollaborationAuditQuery, CollaborationDocumentId,
    CollaborationExchangeMode, CollaborationImportRequest, CollaborationService, StoreError,
    PEER_INDEX_BITS,
};
use serde_json::{json, Value};
use support::{accept, field, key, request, writer, POLICY};

/// A task whose status an agent may not change.
static TASK_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    field(
        "task_id",
        GeneratedCollaborationStorageKind::Scalar,
        Some("task"),
        Some("task_id"),
        GeneratedCollaborationConflict::Immutable,
    ),
    GeneratedCollaborationFieldSpec {
        writers: &[GeneratedWriterConflict {
            kind: ActorKind::Agent,
            conflict: GeneratedCollaborationConflict::Immutable,
        }],
        ..field(
            "status",
            GeneratedCollaborationStorageKind::Scalar,
            Some("task"),
            Some("status"),
            GeneratedCollaborationConflict::Explicit,
        )
    },
    field(
        "notes",
        GeneratedCollaborationStorageKind::Text,
        Some("notes"),
        None,
        GeneratedCollaborationConflict::Merge,
    ),
    field(
        "etag",
        GeneratedCollaborationStorageKind::DerivedRevision,
        None,
        None,
        GeneratedCollaborationConflict::Immutable,
    ),
];
static TASK_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Task",
    id_field: "task_id",
    schema_version: 1,
    authoring_projection: "tasks.authoringState",
    authoring_document: None,
    authoring_store_params: &[],
    import_mutation: "task.importUpdate",
    root_container: "task",
    fields: TASK_FIELDS,
};
static PLANS: &[GeneratedCollaborationEntitySpec] = &[TASK_PLAN];

fn task() -> CollaborationDocumentId {
    CollaborationDocumentId::new("Task", "task-1")
}

fn seed() -> Value {
    json!({ "task_id": "task-1", "status": "open", "notes": "Seeded.", "etag": "" })
}

fn agent() -> Principal {
    Principal::new("planner", ActorKind::Agent)
}

fn service() -> CollaborationService {
    CollaborationService::new(MemoryStorage::default(), PLANS, POLICY, key(), "tasks")
}

fn seeded() -> CollaborationService {
    let service = service();
    service
        .bootstrap(&TASK_PLAN, &task(), &seed(), accept)
        .expect("bootstrap");
    service
}

/// An import of `actor`'s edit setting `path` to `value`, from what the
/// store holds now, written under a block allocated to `actor`.
fn edit(
    service: &CollaborationService,
    actor: &Principal,
    operation_id: &str,
    path: &str,
    value: Value,
) -> CollaborationImportRequest {
    let state = service
        .authoring_state(&TASK_PLAN, &task(), None)
        .expect("authoring state");
    let mut replica = CollaborationReplica::from_versioned_update_base64(
        &TASK_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("replica");
    let block = service.allocate_peers(actor, &task());
    replica.set_peer(block.base).expect("allocated peer");
    let mut edited = replica.materialized_document("edit").expect("document");
    edited[path] = value;
    replica.replace_document(&edited).expect("edit");
    let mut request = request(
        &task(),
        actor,
        &block,
        operation_id,
        &state.accepted_frontier_base64,
        replica
            .export_incremental_update_base64(&state.accepted_frontier_base64)
            .expect("update"),
    );
    request.schema_version = TASK_PLAN.schema_version;
    request
}

fn refusal_code(error: &StoreError) -> Option<&str> {
    error.data.as_ref().and_then(|data| data["code"].as_str())
}

fn refused_paths(error: &StoreError) -> Vec<String> {
    error
        .data
        .as_ref()
        .and_then(|data| data["conflict_paths"].as_array())
        .map(|paths| {
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_accepted_import_records_its_principal_on_its_operation_and_its_peers() {
    let service = seeded();
    let mut request = edit(&service, &writer(), "edit-notes", "notes", json!("Edited."));
    request.intent = Some("record the outcome".to_string());
    let peers = CollaborationReplica::peers_in_update(&request.update_base64).expect("peers");

    service
        .import(&TASK_PLAN, request, accept)
        .expect("an allocated peer's edit");

    let attribution = service
        .attribution(&task())
        .expect("attribution")
        .expect("the task");
    let operation = attribution.operations.last().expect("the edit");
    assert_eq!(operation.operation_id, "edit-notes");
    assert_eq!(operation.actor, writer());
    assert_eq!(operation.intent.as_deref(), Some("record the outcome"));
    for peer in peers {
        assert_eq!(attribution.peers.get(&peer), Some(&writer()), "peer {peer}");
    }
    assert!(
        attribution
            .peers
            .values()
            .all(|principal| *principal == writer() || principal.kind == ActorKind::System),
        "the seed is the store's own write"
    );
}

#[test]
fn operations_under_a_peer_no_nonce_of_the_import_allocated_are_refused() {
    let service = seeded();
    let before = service.summary(&task()).expect("summary");
    let mut request = edit(
        &service,
        &writer(),
        "unallocated",
        "notes",
        json!("Edited."),
    );
    request.peer_nonces = vec![service.allocate_peers(&writer(), &task()).nonce];

    let refused = service
        .import(&TASK_PLAN, request, accept)
        .expect_err("another block");

    assert_eq!(
        refusal_code(&refused),
        Some("collaboration_peer_not_allocated")
    );
    assert_eq!(service.summary(&task()).expect("summary"), before);
}

#[test]
fn a_nonce_allocated_to_another_principal_allocates_nothing_to_the_importer() {
    let service = seeded();
    let mut request = edit(&service, &writer(), "borrowed", "notes", json!("Edited."));
    request.actor = Principal::new("intruder", ActorKind::Human);

    let refused = service
        .import(&TASK_PLAN, request, accept)
        .expect_err("a block allocated to the writer");

    assert_eq!(
        refusal_code(&refused),
        Some("collaboration_peer_not_allocated")
    );
}

#[test]
fn every_peer_of_a_block_is_the_writers_and_one_import_may_name_several_blocks() {
    let service = seeded();
    let state = service
        .authoring_state(&TASK_PLAN, &task(), None)
        .expect("authoring state");
    let first = service.allocate_peers(&writer(), &task());
    let second = service.allocate_peers(&writer(), &task());
    let mut replica = CollaborationReplica::from_versioned_update_base64(
        &TASK_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("replica");
    let last_index = (1 << PEER_INDEX_BITS) - 1;
    for (peer, notes) in [
        (first.base | last_index, "Under the first block."),
        (second.base, "Under the second block."),
    ] {
        replica.set_peer(peer).expect("allocated peer");
        let mut edited = replica.materialized_document("edit").expect("document");
        edited["notes"] = json!(notes);
        replica.replace_document(&edited).expect("edit");
    }
    let mut request = request(
        &task(),
        &writer(),
        &first,
        "two-blocks",
        &state.accepted_frontier_base64,
        replica
            .export_incremental_update_base64(&state.accepted_frontier_base64)
            .expect("update"),
    );
    request.peer_nonces.push(second.nonce);

    service
        .import(&TASK_PLAN, request, accept)
        .expect("both blocks are the writer's");

    let peers = service
        .attribution(&task())
        .expect("attribution")
        .expect("the task")
        .peers;
    assert_eq!(peers.get(&(first.base | last_index)), Some(&writer()));
    assert_eq!(peers.get(&second.base), Some(&writer()));
}

#[test]
fn a_kind_of_writer_a_field_judges_as_immutable_may_not_change_it_unopposed() {
    let service = seeded();

    let refused = service
        .import(
            &TASK_PLAN,
            edit(&service, &agent(), "agent-status", "status", json!("done")),
            accept,
        )
        .expect_err("an agent may not change the status");
    assert_eq!(refused_paths(&refused), ["status"]);

    service
        .import(
            &TASK_PLAN,
            edit(
                &service,
                &agent(),
                "agent-notes",
                "notes",
                json!("Planned."),
            ),
            accept,
        )
        .expect("an agent may change what its kind is not judged otherwise on");
    service
        .import(
            &TASK_PLAN,
            edit(&service, &writer(), "human-status", "status", json!("done")),
            accept,
        )
        .expect("a human changes the status as its own policy allows");
    assert_eq!(
        service
            .detail(&TASK_PLAN, &task())
            .expect("detail")
            .expect("the task")["status"],
        "done"
    );
}

#[test]
fn creating_a_document_is_a_write_judged_by_the_creators_kind() {
    let create = |service: &CollaborationService, actor: &Principal| {
        let block = service.allocate_peers(actor, &task());
        let replica = CollaborationReplica::from_document(&TASK_PLAN, &seed(), block.base)
            .expect("a new task");
        let mut request = request(
            &task(),
            actor,
            &block,
            "create",
            "",
            replica.export_update_base64().expect("update"),
        );
        request.schema_version = TASK_PLAN.schema_version;
        request.exchange_mode = CollaborationExchangeMode::Bootstrap;
        service.import(&TASK_PLAN, request, accept)
    };

    let refused = create(&service(), &agent()).expect_err("an agent may not set the status");
    assert_eq!(refused_paths(&refused), ["status"]);
    create(&service(), &writer()).expect("a human creates the task");
}

#[test]
fn every_refused_import_is_audited_under_its_principal() {
    let service = seeded();
    let refused = edit(&service, &agent(), "agent-status", "status", json!("done"));
    let digest_input = refused.update_base64.clone();
    service
        .import(&TASK_PLAN, refused, accept)
        .expect_err("refused");
    service
        .import(
            &TASK_PLAN,
            edit(&service, &writer(), "invalid", "notes", json!("")),
            |task: &Value| match task["notes"].as_str() {
                Some("") => Err(StoreError::invalid_request("Notes are required.")),
                _ => Ok(()),
            },
        )
        .expect_err("invalid");
    service
        .import(
            &TASK_PLAN,
            edit(&service, &writer(), "accepted", "notes", json!("Kept.")),
            accept,
        )
        .expect("accepted");

    let audit = service
        .audit(&CollaborationAuditQuery {
            document: Some(task()),
            ..CollaborationAuditQuery::default()
        })
        .expect("audit");
    let refusals: Vec<_> = audit
        .iter()
        .map(|record| match &record.event {
            CollaborationAuditEvent::Refusal {
                operation_id, code, ..
            } => (record.actor.clone(), operation_id.clone(), code.clone()),
            event => panic!("only imports were made, not {event:?}"),
        })
        .collect();
    assert_eq!(
        refusals,
        [
            (
                agent(),
                "agent-status".to_string(),
                "collaboration_policy".to_string()
            ),
            (
                writer(),
                "invalid".to_string(),
                "collaboration_document_invalid".to_string()
            ),
        ]
    );
    let CollaborationAuditEvent::Refusal {
        conflict_paths,
        update_base64_sha256,
        ..
    } = &audit[0].event
    else {
        unreachable!("checked above");
    };
    assert_eq!(conflict_paths, &["status"]);
    assert_eq!(update_base64_sha256.len(), 64, "a SHA-256 digest");
    assert_ne!(update_base64_sha256, &digest_input, "not the update itself");

    let by_agent = service
        .audit(&CollaborationAuditQuery {
            actor_id: Some(agent().id),
            ..CollaborationAuditQuery::default()
        })
        .expect("audit");
    assert_eq!(by_agent, audit[..1]);
}

#[test]
fn the_audit_answers_a_time_range_and_keeps_what_retention_leaves() {
    let service = seeded();
    service
        .import(
            &TASK_PLAN,
            edit(&service, &agent(), "agent-status", "status", json!("done")),
            accept,
        )
        .expect_err("refused");
    let recorded = service
        .audit(&CollaborationAuditQuery::default())
        .expect("audit")[0]
        .timestamp_unix_ms;

    let range = |from: u128, until: u128| {
        service
            .audit(&CollaborationAuditQuery {
                from_unix_ms: Some(from),
                until_unix_ms: Some(until),
                ..CollaborationAuditQuery::default()
            })
            .expect("audit")
            .len()
    };
    assert_eq!(range(recorded, recorded + 1), 1);
    assert_eq!(range(recorded + 1, recorded + 2), 0);
    assert_eq!(range(recorded - 1, recorded), 0);

    assert_eq!(
        service
            .discard_audit_before(SystemTime::now() - Duration::from_secs(3600))
            .expect("discard"),
        0
    );
    assert_eq!(
        service
            .discard_audit_before(SystemTime::now() + Duration::from_secs(1))
            .expect("discard"),
        1
    );
    assert!(service
        .audit(&CollaborationAuditQuery::default())
        .expect("audit")
        .is_empty());
}

#[test]
fn a_store_without_a_long_enough_key_cannot_be_built() {
    assert!(clerkenwell_store::PeerKey::new([0; 31]).is_err());
}

use super::*;
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationFieldSpec,
    GeneratedCollaborationStorageKind, GeneratedCollaborationValueCodec,
};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use tempfile::TempDir;

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
        conflict,
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
        substrate: "loro",
        schema_version: 1,
        migration_ids: &[],
        authoring_projection: "authoringState",
        import_mutation: "importLoroUpdate",
        root_container,
        fields,
    }
}

use GeneratedCollaborationConflict::{Explicit, Immutable, Merge};
use GeneratedCollaborationStorageKind::{DerivedRevision, Scalar, Text};

static NOTE_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    field("note_id", Scalar, Some("note"), Some("note_id"), Immutable),
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
static NOTE_PLAN: GeneratedCollaborationEntitySpec = plan("Note", "note_id", "note", NOTE_FIELDS);
static TASK_PLAN: GeneratedCollaborationEntitySpec = plan("Task", "task_id", "task", TASK_FIELDS);
static PLANS: &[GeneratedCollaborationEntitySpec] = &[NOTE_PLAN, TASK_PLAN];

/// A validator that accepts every document.
fn accept(_: &Value) -> StoreResult<()> {
    Ok(())
}

const RELATIVE_PATH: &str = "notes/fixture.yaml";

fn note_seed() -> Value {
    json!({
        "note_id": "note-1",
        "title": "Shopping",
        "body": "Bread and milk.",
        "summary": "Groceries.",
        "etag": "",
    })
}

fn open_service(root: &Path) -> CollaborationService {
    CollaborationService::new(root, PLANS)
}

fn initialise(
    root: &Path,
) -> (
    CollaborationService,
    CollaborationDocumentId,
    Value,
    CollaborationAuthoringState,
) {
    let service = open_service(root);
    let seed = note_seed();
    let resource_id = seed["note_id"].as_str().expect("note ID").to_string();
    let document = CollaborationDocumentId::new("Note", resource_id);
    service
        .bootstrap(&NOTE_PLAN, &document, RELATIVE_PATH, &seed, accept)
        .expect("bootstrap collaboration document");
    let state = service
        .authoring_state(&NOTE_PLAN, &document)
        .expect("read collaboration document");
    (service, document, seed, state)
}

#[test]
fn reading_missing_collaboration_documents_does_not_create_storage_or_lock_files() {
    let temp = tempfile::tempdir().unwrap();
    let storage = LocalFileCollaborationStorage::new(temp.path(), PLANS);
    let id = CollaborationDocumentId::new("Note", "missing");
    for _ in 0..3 {
        assert!(storage.load(&id).unwrap().is_none());
        assert!(storage.list_entity("Note").unwrap().is_empty());
        assert!(!storage.verify(&id).valid);
    }
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn concurrent_explicit_scalar_edits_block_until_rebased_resolution() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, seed, state) = initialise(root.path());
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
        .authoring_state(&NOTE_PLAN, &document)
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
    let mut client = LoroAuthoringDocument::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .expect("hydrate client");
    client.replace_document(&edited).expect("edit client");
    CollaborationImportRequest {
        document: document.clone(),
        relative_path: RELATIVE_PATH.to_string(),
        schema_version: state.schema_version,
        operation_id: operation_id.to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: state.accepted_frontier_base64.clone(),
        update_base64: client
            .export_incremental_update_base64(&state.accepted_frontier_base64)
            .expect("incremental update"),
        fence: ImportFence::Frontier,
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
fn captured_text_consumption_preserves_later_edits_across_restart_and_retry() {
    let root = TempDir::new().unwrap();
    let (service, document, seed, state) = initialise(root.path());
    let captured = LoroAuthoringDocument::from_versioned_update_base64(
        &NOTE_PLAN,
        state.schema_version,
        &state.update_base64,
    )
    .unwrap();
    let path = "body";
    let text = captured
        .text_at_frontier(path, &HashMap::new(), &state.accepted_frontier_base64)
        .unwrap();
    let prepared = captured
        .prepare_text_replacement_at_frontier(
            path,
            &HashMap::new(),
            &state.accepted_frontier_base64,
            &text,
            "",
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
        )
        .unwrap();
    import(
        &service,
        CollaborationImportRequest {
            document: document.clone(),
            relative_path: RELATIVE_PATH.to_string(),
            schema_version: state.schema_version,
            operation_id: "later-text".to_string(),
            exchange_mode: CollaborationExchangeMode::Incremental,
            base_frontier_base64: state.accepted_frontier_base64.clone(),
            update_base64: replacement,
            fence: ImportFence::Frontier,
        },
        &seed,
    )
    .unwrap();
    let request = CollaborationImportRequest {
        document: document.clone(),
        relative_path: RELATIVE_PATH.to_string(),
        schema_version: state.schema_version,
        operation_id: "captured-consumption".to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: state.accepted_frontier_base64.clone(),
        update_base64: prepared,
        fence: ImportFence::Frontier,
    };
    let before = service.authoring_state(&NOTE_PLAN, &document).unwrap();
    // Rejection must leave accepted text and the durable frontier untouched.
    assert!(service
        .import(&NOTE_PLAN, request.clone(), |_| {
            Err(StoreError::invalid_request("Rejected test acceptance"))
        })
        .is_err());
    assert_eq!(
        service
            .authoring_state(&NOTE_PLAN, &document)
            .unwrap()
            .accepted_frontier_base64,
        before.accepted_frontier_base64
    );
    import(&service, request.clone(), &seed).unwrap();
    drop(service);
    let restarted = open_service(root.path());
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
        CollaborationFaultPoint::BeforeTemporaryWrite,
        CollaborationFaultPoint::AfterTemporarySync,
    ] {
        let root = TempDir::new().expect("temp workspace");
        let (service, document, seed, state) = initialise(root.path());
        let before = service
            .storage()
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
        service.storage().inject_fault(point);

        let error = import(&service, request, &seed).expect_err("fault must interrupt import");
        assert_eq!(
            error.data.as_ref().and_then(|data| data["code"].as_str()),
            Some("collaboration_fault_injected")
        );
        let after = service
            .storage()
            .load(&document)
            .expect("read after fault")
            .expect("baseline remains");
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.checkpoint_sha256, before.checkpoint_sha256);
        assert_eq!(after.publication_pending, before.publication_pending);
    }
}

#[test]
fn post_commit_faults_leave_the_new_generation_recoverably_pending() {
    for point in [
        CollaborationFaultPoint::AfterRenameBeforeDirectorySync,
        CollaborationFaultPoint::AfterDurableCommit,
    ] {
        let root = TempDir::new().expect("temp workspace");
        let (service, document, seed, state) = initialise(root.path());
        let before = service
            .storage()
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
        service.storage().inject_fault(point);

        import(&service, request, &seed).expect_err("fault reports interrupted publication");
        let after = LocalFileCollaborationStorage::new(root.path(), PLANS)
            .load(&document)
            .expect("restart reads committed generation")
            .expect("committed envelope");
        assert!(after.generation > before.generation);
        assert!(after.publication_pending);
        let materialized = service
            .detail(&NOTE_PLAN, &document)
            .expect("materialize accepted generation")
            .expect("accepted detail");
        assert_eq!(materialized["body"], replacement);
    }
}

#[test]
fn process_loss_before_publication_ack_is_recovered_exactly_once() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, seed, state) = initialise(root.path());
    let request = edit_request(
        &document,
        &seed,
        &state,
        "publication-loss",
        &["summary"],
        json!("accepted before process loss"),
    );
    let accepted = import(&service, request, &seed).expect("durable import");
    service
        .storage()
        .inject_fault(CollaborationFaultPoint::BeforePublicationAcknowledgement);
    service
        .acknowledge_publication(&document, accepted.generation)
        .expect_err("simulated process loss");

    let restarted = open_service(root.path());
    let pending = restarted
        .publications()
        .expect("restart publication scan")
        .into_iter()
        .find(|publication| publication.document == document)
        .expect("accepted operation remains discoverable");
    assert!(pending.pending);
    restarted
        .acknowledge_publication(&document, pending.generation)
        .expect("publish after restart");
    restarted
        .acknowledge_publication(&document, pending.generation)
        .expect("duplicate acknowledgement is idempotent");
    assert!(
        !restarted
            .storage()
            .load(&document)
            .expect("read acknowledged state")
            .expect("collaboration state")
            .publication_pending
    );
}

#[test]
fn publication_scan_reads_identity_without_materialising_authoritative_payloads() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, _seed, _state) = initialise(root.path());
    let path = service.storage().envelope_path(&document);
    let mut persisted: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read durable collaboration envelope"))
            .expect("parse durable collaboration envelope");
    persisted["checkpoint_update_base64"] = json!("not valid base64");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&persisted).expect("serialise corrupted envelope"),
    )
    .expect("write corrupted collaboration envelope");

    let publications = service
        .publications()
        .expect("publication identity remains observable");
    assert!(
        publications
            .iter()
            .any(|publication| publication.document == document),
        "publication scan must observe document identity without decoding its payload"
    );

    let error = service
        .storage()
        .load(&document)
        .expect_err("authoritative reads must still validate payload integrity");
    assert_eq!(
        error.data.as_ref().and_then(|data| data["code"].as_str()),
        Some("collaboration_state_corrupt")
    );
}

#[test]
fn publication_scan_isolates_an_invalid_envelope_from_healthy_publications() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, _seed, _state) = initialise(root.path());
    let invalid_path = service.storage().entity_dir("Note").join("invalid.json");
    std::fs::write(&invalid_path, b"not json").expect("write invalid envelope");

    let scan = service.publication_scan().expect("best-effort scan");
    assert!(
        scan.publications
            .iter()
            .any(|publication| publication.document == document),
        "a malformed neighbour must not hide a healthy publication"
    );
    assert_eq!(scan.problems.len(), 1);
    assert_eq!(
        scan.problems[0].code,
        "collaboration_publication_invalid_json"
    );
    assert_eq!(scan.problems[0].source, invalid_path.display().to_string());
}

#[test]
fn two_process_adapters_serialise_concurrent_commits_and_converge() {
    let root = TempDir::new().expect("temp workspace");
    let (process_a, document, seed, state) = initialise(root.path());
    let process_b = open_service(root.path());
    let request_a = edit_request(
        &document,
        &seed,
        &state,
        "process-a",
        &["body"],
        json!("edit from process A"),
    );
    let replay_a = request_a.clone();
    let request_b = edit_request(
        &document,
        &seed,
        &state,
        "process-b",
        &["summary"],
        json!("edit from process B"),
    );
    let seed_a = seed.clone();
    let seed_b = seed.clone();
    let thread_a = std::thread::spawn(move || import(&process_a, request_a, &seed_a));
    let process_b_import = process_b.clone();
    let thread_b = std::thread::spawn(move || import(&process_b_import, request_b, &seed_b));
    let accepted_a = thread_a
        .join()
        .expect("process A thread")
        .expect("process A");
    let accepted_b = thread_b
        .join()
        .expect("process B thread")
        .expect("process B");

    assert_ne!(accepted_a.generation, accepted_b.generation);
    let converged_a = open_service(root.path())
        .detail(&NOTE_PLAN, &document)
        .expect("process A projection")
        .expect("process A detail");
    let converged_b = process_b
        .detail(&NOTE_PLAN, &document)
        .expect("process B projection")
        .expect("process B detail");
    assert_eq!(converged_a, converged_b);
    assert_eq!(converged_a["body"], json!("edit from process A"));
    assert_eq!(converged_a["summary"], json!("edit from process B"));

    let duplicate = import(&process_b, replay_a, &seed).expect("cross-process retry");
    assert!(duplicate.duplicate);
    let envelope = process_b
        .storage()
        .load(&document)
        .expect("retained window")
        .expect("envelope");
    assert!(envelope.retained_operations.len() >= 3);
    assert!(envelope.projection_revision_floor() > 1);
}

#[test]
fn corruption_is_reported_and_checkpoint_repair_preserves_evidence() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, _seed, _) = initialise(root.path());
    let path = service.storage().envelope_path(&document);
    let original = std::fs::read(&path).expect("original envelope");
    let mut envelope: DurableCollaborationEnvelope =
        serde_json::from_slice(&original).expect("parse envelope");
    envelope.retained_operations[0].update_sha256 = "corrupt".to_string();
    let corrupt_bytes = serde_json::to_vec_pretty(&envelope).expect("encode corrupt envelope");
    std::fs::write(&path, &corrupt_bytes).expect("install corruption");

    let inspection = service.verify(&document);
    assert!(!inspection.valid);
    assert!(service.detail(&NOTE_PLAN, &document).is_err());
    let evidence = service
        .repair(&document, "checksum verification failed")
        .expect("repair from valid checkpoint");
    assert_eq!(
        std::fs::read(&evidence).expect("evidence bytes"),
        corrupt_bytes
    );
    assert!(service.verify(&document).valid);
    assert!(service
        .detail(&NOTE_PLAN, &document)
        .expect("materialize repaired checkpoint")
        .is_some());
}

#[test]
fn operator_recovery_requests_are_durably_audited_without_reason_text() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, _, _) = initialise(root.path());
    let private_reason = "operator supplied private incident context";

    service
        .export_for_recovery(&document)
        .expect("audited export");
    service
        .quarantine(&document, private_reason)
        .expect("audited quarantine");
    service.reindex().expect("audited reindex");
    service.reset(&document).expect("audited destructive reset");

    let records = service.recovery_audit().expect("read recovery audit");
    assert_eq!(
        records
            .iter()
            .map(|record| record.action)
            .collect::<Vec<_>>(),
        vec![
            CollaborationRecoveryAction::Export,
            CollaborationRecoveryAction::Quarantine,
            CollaborationRecoveryAction::Reindex,
            CollaborationRecoveryAction::Reset,
        ]
    );
    assert!(records.last().is_some_and(|record| record.destructive));
    assert!(records
        .iter()
        .all(|record| { record.reason_sha256.as_deref() != Some(private_reason) }));
    assert!(!std::fs::read_to_string(service.storage().audit_path())
        .expect("audit text")
        .contains(private_reason));
}

/// A structurally valid envelope whose checkpoint is not a Loro document:
/// enough for anything that reads envelopes without materialising them.
fn opaque_envelope(
    entity: &str,
    resource_id: &str,
    schema_version: u32,
    publication_pending: bool,
) -> DurableCollaborationEnvelope {
    let checkpoint = resource_id.as_bytes();
    DurableCollaborationEnvelope {
        envelope_version: ENVELOPE_VERSION,
        entity: entity.to_string(),
        resource_id: resource_id.to_string(),
        relative_path: resource_id.to_string(),
        schema_version,
        generation: 1,
        checkpoint_sequence: 0,
        compacted_through_sequence: 0,
        checkpoint_update_base64: BASE64.encode(checkpoint),
        checkpoint_sha256: sha256_hex(checkpoint),
        checkpoint_bytes: checkpoint.len(),
        retained_operations: Vec::new(),
        publication_pending,
        pending_rename_from: None,
    }
}

fn files_under(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(root).expect("read directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.insert(path.clone(), std::fs::read(&path).expect("read file"));
        }
    }
    files
}

/// Every file under `root` with its bytes and modification time.
fn file_states(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    files_under(root)
        .into_iter()
        .map(|(path, bytes)| {
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .expect("file modification time");
            (path, (bytes, modified))
        })
        .collect()
}

#[test]
fn reads_leave_every_stored_byte_and_timestamp_unchanged() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, seed, state) = initialise(root.path());
    let request = edit_request(
        &document,
        &seed,
        &state,
        "read-purity-edit",
        &["body"],
        json!("an accepted edit"),
    );
    let accepted = import(&service, request, &seed).expect("accepted edit");
    service
        .acknowledge_publication(&document, accepted.generation)
        .expect("publish");
    let before = file_states(root.path());

    for _ in 0..2 {
        service.detail(&NOTE_PLAN, &document).expect("detail");
        service
            .authoring_state(&NOTE_PLAN, &document)
            .expect("authoring state");
        service.storage().load(&document).expect("load");
        service.storage().list_entity("Note").expect("list entity");
        service.publication_scan().expect("publication scan");
        service.publications().expect("publications");
        service.inspect(None, None, None).expect("inspect");
        service.verify(&document);
        service.recovery_audit().expect("recovery audit");
        service.counters();
    }

    assert_eq!(file_states(root.path()), before);
}

#[test]
fn reindex_reads_every_document_and_changes_nothing_but_the_audit() {
    let root = TempDir::new().expect("temp workspace");
    let service = open_service(root.path());
    let count = 250;
    for index in 0..count {
        service
            .storage()
            .install_migrated(opaque_envelope(
                PLANS[index % PLANS.len()].name,
                &format!("document-{index:03}"),
                1,
                false,
            ))
            .expect("install envelope");
    }
    let before = files_under(root.path());

    let (documents, revision) = service.reindex().expect("reindex");
    let (again, same_revision) = service.reindex().expect("reindex again");

    assert_eq!(documents, count);
    assert_eq!(again, count);
    assert_eq!(
        revision, same_revision,
        "an unchanged catalogue keeps its fingerprint"
    );
    let after = files_under(root.path());
    let audit = service.storage().audit_path();
    let changed = after
        .iter()
        .filter(|(path, bytes)| before.get(*path) != Some(bytes))
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    assert!(
        changed
            .iter()
            .all(|path| path == &audit || path == &service.storage().audit_lock_path()),
        "reindex wrote more than its audit record: {changed:?}"
    );
    assert!(before.keys().all(|path| after.contains_key(path)));
}

#[test]
fn inspection_reports_a_required_migration_for_every_generated_plan() {
    for spec in PLANS {
        let envelope =
            |schema_version| opaque_envelope(spec.name, "resource", schema_version, false);
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
    let root = TempDir::new().expect("temp workspace");
    let (service, document, _, _) = initialise(root.path());
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
    let root = TempDir::new().expect("temp workspace");
    let (service, document, seed, state) = initialise(root.path());
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
    let root = TempDir::new().expect("temp workspace");
    let (service, document, seed, state) = initialise(root.path());
    let first = edit_request(
        &document,
        &seed,
        &state,
        "first",
        &["summary"],
        json!("first"),
    );
    import(&service, first, &seed).expect("an earlier writer commits");
    let before = service.storage().load(&document).unwrap().unwrap();
    let stale = on_read(
        edit_request(&document, &seed, &state, "stale", &["body"], json!("stale")),
        &state,
    );

    let error = import(&service, stale, &seed).expect_err("the read was superseded");

    assert_eq!(error.kind, StoreErrorKind::Conflict);
    let data = error.data.expect("the refusal carries data");
    let accepted = service.authoring_state(&NOTE_PLAN, &document).unwrap();
    assert_eq!(data["conflict_kind"], "collaboration_revision");
    assert_eq!(data["expected_etag"], json!(state.etag));
    assert_eq!(data["current_etag"], json!(accepted.etag));
    assert_eq!(data[NOTE_PLAN.id_field], json!(document.resource_id));
    assert_eq!(data["current"]["summary"], json!("first"));
    let after = service.storage().load(&document).unwrap().unwrap();
    assert_eq!(after.generation, before.generation);
    assert_eq!(after.checkpoint_sha256, before.checkpoint_sha256);
}

#[test]
fn a_retried_etag_fenced_import_is_a_duplicate_not_a_stale_read() {
    let root = TempDir::new().expect("temp workspace");
    let (service, document, seed, state) = initialise(root.path());
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
fn etag_fenced_imports_racing_from_one_read_in_two_processes_commit_exactly_once() {
    let root = TempDir::new().expect("temp workspace");
    let (process_a, document, seed, state) = initialise(root.path());
    let process_b = open_service(root.path());
    let request_a = on_read(
        edit_request(
            &document,
            &seed,
            &state,
            "race-a",
            &["body"],
            json!("from process A"),
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
            json!("from process B"),
        ),
        &state,
    );
    let (seed_a, seed_b) = (seed.clone(), seed.clone());
    let thread_a = std::thread::spawn(move || import(&process_a, request_a, &seed_a));
    let thread_b = std::thread::spawn(move || import(&process_b, request_b, &seed_b));
    let outcomes = [
        thread_a.join().expect("process A thread"),
        thread_b.join().expect("process B thread"),
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

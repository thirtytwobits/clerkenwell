//! The collaboration service commits through a storage port implemented
//! outside this crate that keeps nothing but bytes and versions.

mod support;

use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationRecoveryAction, CollaborationRecoveryAuditRecord, CollaborationService,
    CollaborationStoragePort, CommitPolicy, DurableCollaborationEnvelope, ImportFence,
    LocalFileCollaborationStorage, StoreResult, StoredEnvelope,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use support::{accept, NOTE_PLAN, PLANS, POLICY};

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct MemoryState {
    next_version: u64,
    envelopes: BTreeMap<String, (String, Vec<u8>)>,
    evidence: Vec<Vec<u8>>,
    audit: Vec<CollaborationRecoveryAuditRecord>,
    /// Runs once, after the next write to the named source has landed.
    after_write: Option<(String, Hook)>,
    /// Whether every compare-and-swap reports that another writer won.
    lose_swaps: bool,
    /// How many compare-and-swaps were asked for.
    swaps: usize,
}

impl std::fmt::Debug for MemoryState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryState")
            .field("envelopes", &self.envelopes.keys())
            .finish()
    }
}

/// A port that keeps each document's bytes in memory under a counter
/// version, with no knowledge of what the bytes hold.
#[derive(Debug, Clone, Default)]
struct MemoryPort(Arc<Mutex<MemoryState>>);

impl MemoryPort {
    fn state(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.0.lock().expect("memory port")
    }

    /// Runs `hook` once, after the next write to `document` lands.
    fn after_write(&self, document: &CollaborationDocumentId, hook: Hook) {
        self.state().after_write = Some((self.source(document), hook));
    }

    fn run_hook(&self, source: &str) {
        let hook = {
            let mut state = self.state();
            match state.after_write.take() {
                Some((target, hook)) if target == source => Some(hook),
                other => {
                    state.after_write = other;
                    None
                }
            }
        };
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl CollaborationStoragePort for MemoryPort {
    fn source(&self, document: &CollaborationDocumentId) -> String {
        format!("{}/{}", document.entity, document.resource_id)
    }

    fn sources(&self, entity: &str) -> StoreResult<Vec<String>> {
        let prefix = format!("{entity}/");
        Ok(self
            .state()
            .envelopes
            .keys()
            .filter(|source| source.starts_with(&prefix))
            .cloned()
            .collect())
    }

    fn read(&self, source: &str) -> StoreResult<Option<StoredEnvelope>> {
        let state = self.state();
        Ok(state
            .envelopes
            .get(source)
            .map(|(version, bytes)| StoredEnvelope {
                version: version.clone(),
                bytes: bytes.clone(),
            }))
    }

    fn stamp(&self, source: &str) -> StoreResult<Option<String>> {
        Ok(self
            .state()
            .envelopes
            .get(source)
            .map(|(version, _)| version.clone()))
    }

    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>> {
        let source = self.source(document);
        let version = {
            let mut state = self.state();
            state.swaps += 1;
            if state.lose_swaps {
                return Ok(None);
            }
            let current = state
                .envelopes
                .get(&source)
                .map(|(version, _)| version.as_str());
            if current != expected {
                return Ok(None);
            }
            state.next_version += 1;
            let version = state.next_version.to_string();
            state
                .envelopes
                .insert(source.clone(), (version.clone(), bytes.to_vec()));
            version
        };
        self.run_hook(&source);
        Ok(Some(version))
    }

    fn compare_and_remove(
        &self,
        document: &CollaborationDocumentId,
        expected: &str,
    ) -> StoreResult<bool> {
        let source = self.source(document);
        let mut state = self.state();
        if state
            .envelopes
            .get(&source)
            .map(|(version, _)| version.as_str())
            != Some(expected)
        {
            return Ok(false);
        }
        state.envelopes.remove(&source);
        Ok(true)
    }

    fn preserve(
        &self,
        _document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<String> {
        let mut state = self.state();
        state.evidence.push(bytes.to_vec());
        Ok(format!("evidence/{}-{label}", state.evidence.len()))
    }

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()> {
        self.state().audit.push(record.clone());
        Ok(())
    }

    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        Ok(self.state().audit.clone())
    }
}

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
        LoroAuthoringDocument::from_document(&NOTE_PLAN, &seed_document()).expect("seed client");
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

/// Drives one document through commits, a retry, a rename and recovery,
/// recording what the service reports.
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
    observed.push(history(&service.load(&document).unwrap().unwrap()));

    let renamed = CollaborationDocumentId::new("Note", "note-2");
    service
        .move_document(&NOTE_PLAN, &document, &renamed, accept)
        .expect("move")
        .expect("the document exists");
    assert!(
        service.load(&document).unwrap().is_none(),
        "the source is gone"
    );
    observed.push(history(&service.load(&renamed).unwrap().unwrap()));
    // The move writes its operation from a replica of its own, whose peer
    // differs from one service to the next, and so does the etag it leaves.
    let mut moved = service.detail(&NOTE_PLAN, &renamed).unwrap().unwrap();
    moved.as_object_mut().expect("a note").remove("etag");
    observed.push(moved);

    service.repair(&renamed, "drop the window").expect("repair");
    observed.push(history(&service.load(&renamed).unwrap().unwrap()));
    assert!(service.verify(&renamed).valid);
    let exported = service.export_for_recovery(&renamed).expect("export");
    assert!(!exported.is_empty());
    service
        .quarantine(&renamed, "evidence")
        .expect("quarantine");
    service.reset(&renamed).expect("reset");
    assert!(service.load(&renamed).unwrap().is_none());
    observed.push(json!(service
        .recovery_audit()
        .unwrap()
        .iter()
        .map(|record| record.action)
        .collect::<Vec<_>>()));
    Value::Array(observed)
}

#[test]
fn a_port_that_keeps_only_bytes_carries_the_protocol_as_the_file_store_does() {
    let edits = edits(12);
    let root = tempfile::tempdir().expect("temp store");
    let on_files = drive(
        &CollaborationService::with_storage(
            LocalFileCollaborationStorage::new(root.path()),
            PLANS,
            POLICY,
            "file-port",
        ),
        &edits,
    );
    let in_memory = drive(
        &CollaborationService::with_storage(MemoryPort::default(), PLANS, POLICY, "memory-port"),
        &edits,
    );

    assert_eq!(in_memory, on_files);
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
    let port = MemoryPort::default();
    let service = CollaborationService::with_storage(port.clone(), PLANS, POLICY, "memory-port");
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
    assert_eq!(port.state().evidence, vec![stored.bytes]);
}

#[test]
fn commits_racing_through_one_port_both_land() {
    let port = MemoryPort::default();
    let first = CollaborationService::with_storage(port.clone(), PLANS, POLICY, "memory-port");
    let second = CollaborationService::with_storage(port, PLANS, POLICY, "memory-port");
    let document = note();
    first
        .bootstrap(&NOTE_PLAN, &document, &seed_document(), accept)
        .expect("seed");
    let state = first
        .authoring_state(&NOTE_PLAN, &document, None)
        .expect("read");
    let edit = |body: &str| {
        let mut client = LoroAuthoringDocument::from_versioned_update_base64(
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
fn a_move_takes_the_source_as_it_stands_when_another_writer_commits_during_it() {
    let edits = edits(1);
    let port = MemoryPort::default();
    let service = CollaborationService::with_storage(port.clone(), PLANS, POLICY, "memory-port");
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
    let renamed = CollaborationDocumentId::new("Note", "note-2");
    let (operation_id, base, update) = edits.edits[0].clone();
    let writer = service.clone();
    port.after_write(
        &renamed,
        Box::new(move || {
            writer
                .import(&NOTE_PLAN, request(&operation_id, &base, &update), accept)
                .expect("a concurrent commit to the source");
        }),
    );

    service
        .move_document(&NOTE_PLAN, &document, &renamed, accept)
        .expect("move")
        .expect("the document exists");

    assert!(
        service.load(&document).unwrap().is_none(),
        "the source is gone"
    );
    let moved = service.load(&renamed).unwrap().expect("moved");
    assert!(moved
        .retained_operations
        .iter()
        .any(|operation| operation.operation_id == edits.edits[0].0));
    assert_eq!(
        service.detail(&NOTE_PLAN, &renamed).unwrap().unwrap()["body"],
        json!("Edit 0.")
    );
}

#[test]
fn an_import_that_keeps_losing_its_commit_is_refused_after_the_attempts_its_policy_allows() {
    let edits = edits(1);
    let port = MemoryPort::default();
    let policy = CommitPolicy {
        attempts: NonZeroU32::new(3).expect("attempts"),
        ..POLICY
    };
    let service = CollaborationService::with_storage(port.clone(), PLANS, policy, "memory-port");
    service
        .bootstrap_update(
            &NOTE_PLAN,
            &note(),
            NOTE_PLAN.schema_version,
            &edits.seed_update,
            accept,
        )
        .expect("seed");
    {
        let mut state = port.state();
        state.lose_swaps = true;
        state.swaps = 0;
    }

    let (operation_id, base, update) = &edits.edits[0];
    let refused = service
        .import(&NOTE_PLAN, request(operation_id, base, update), accept)
        .expect_err("every commit loses");

    assert_eq!(
        refused.data.as_ref().and_then(|data| data["code"].as_str()),
        Some("collaboration_commit_contended")
    );
    assert_eq!(port.state().swaps, policy.attempts.get() as usize);
}

#[test]
fn an_envelope_keeps_the_latest_operations_its_policy_retains() {
    let edits = edits(6);
    let policy = CommitPolicy {
        retained_operations: 3,
        ..POLICY
    };
    let service =
        CollaborationService::with_storage(MemoryPort::default(), PLANS, policy, "memory-port");
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

    let kept = service
        .load(&note())
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

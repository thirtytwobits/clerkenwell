//! The collaboration service commits through a storage port implemented
//! outside this crate that keeps nothing but bytes and versions.

mod support;

use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationPublicationScanCache, CollaborationRecoveryAction,
    CollaborationRecoveryAuditRecord, CollaborationService, CollaborationStoragePort,
    DurableCollaborationEnvelope, ImportFence, LocalFileCollaborationStorage, StoreResult,
    StoredEnvelope,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use support::{accept, NOTE_PLAN, PLANS};

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct MemoryState {
    next_version: u64,
    envelopes: BTreeMap<String, (String, Vec<u8>)>,
    evidence: Vec<Vec<u8>>,
    audit: Vec<CollaborationRecoveryAuditRecord>,
    /// How many reads asked for a version as well as the bytes.
    versioned_reads: usize,
    /// How many reads asked for the bytes alone.
    byte_reads: usize,
    /// Runs once, after the next write to the named source has landed.
    after_write: Option<(String, Hook)>,
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
        let mut state = self.state();
        state.versioned_reads += 1;
        Ok(state
            .envelopes
            .get(source)
            .map(|(version, bytes)| StoredEnvelope {
                version: version.clone(),
                bytes: bytes.clone(),
            }))
    }

    fn read_bytes(&self, source: &str) -> StoreResult<Option<Vec<u8>>> {
        let mut state = self.state();
        state.byte_reads += 1;
        Ok(state.envelopes.get(source).map(|(_, bytes)| bytes.clone()))
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
    ) -> StoreResult<PathBuf> {
        let mut state = self.state();
        state.evidence.push(bytes.to_vec());
        Ok(PathBuf::from(format!(
            "evidence/{}-{label}",
            state.evidence.len()
        )))
    }

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()> {
        self.state().audit.push(record.clone());
        Ok(())
    }

    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        Ok(self.state().audit.clone())
    }
}

const RELATIVE_PATH: &str = "notes/note-1.yaml";

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
        relative_path: RELATIVE_PATH.to_string(),
        schema_version: NOTE_PLAN.schema_version,
        operation_id: operation_id.to_string(),
        exchange_mode: CollaborationExchangeMode::Incremental,
        base_frontier_base64: base.to_string(),
        update_base64: update.to_string(),
        fence: ImportFence::Frontier,
    }
}

/// What an envelope says about a document's history and publication,
/// leaving out how its checkpoint is encoded.
fn history(envelope: &DurableCollaborationEnvelope) -> Value {
    json!({
        "resource_id": envelope.resource_id,
        "relative_path": envelope.relative_path,
        "schema_version": envelope.schema_version,
        "generation": envelope.generation,
        "checkpoint_sequence": envelope.checkpoint_sequence,
        "compacted_through_sequence": envelope.compacted_through_sequence,
        "retained": envelope
            .retained_operations
            .iter()
            .map(|operation| json!([operation.operation_id, operation.sequence]))
            .collect::<Vec<_>>(),
        "publication_pending": envelope.publication_pending,
        "pending_rename_from": envelope.pending_rename_from,
    })
}

/// Drives one document through commits, a retry, publication, a path
/// change, a rename and recovery, recording what the service reports.
fn drive<S: CollaborationStoragePort>(service: &CollaborationService<S>, edits: &Edits) -> Value {
    let mut observed = Vec::new();
    let document = note();
    service
        .bootstrap_update(
            &NOTE_PLAN,
            &document,
            RELATIVE_PATH,
            NOTE_PLAN.schema_version,
            &edits.seed_update,
            accept,
        )
        .expect("seed");
    let mut generation = 0;
    for (operation_id, base, update) in &edits.edits {
        let imported = service
            .import(&NOTE_PLAN, request(operation_id, base, update), accept)
            .expect("commit");
        assert!(!imported.duplicate, "{operation_id} is new");
        generation = imported.generation;
    }
    let (operation_id, base, update) = edits.edits.last().expect("an edit");
    let retried = service
        .import(&NOTE_PLAN, request(operation_id, base, update), accept)
        .expect("retry");
    assert!(retried.duplicate, "a retried operation is a duplicate");
    observed.push(history(&service.load(&document).unwrap().unwrap()));

    service
        .acknowledge_publication(&document, generation)
        .expect("acknowledge");
    observed.push(history(&service.load(&document).unwrap().unwrap()));
    service
        .update_relative_path(&document, "notes/moved-note-1.yaml")
        .expect("path")
        .expect("the document exists");
    observed.push(history(&service.load(&document).unwrap().unwrap()));

    let renamed = CollaborationDocumentId::new("Note", "note-2");
    service
        .move_document(&document, &renamed, "notes/note-2.yaml")
        .expect("move")
        .expect("the document exists");
    assert!(
        service.load(&document).unwrap().is_none(),
        "the source is gone"
    );
    observed.push(history(&service.load(&renamed).unwrap().unwrap()));
    observed.push(service.detail(&NOTE_PLAN, &renamed).unwrap().unwrap());

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
        &CollaborationService::with_storage(LocalFileCollaborationStorage::new(root.path()), PLANS),
        &edits,
    );
    let in_memory = drive(
        &CollaborationService::with_storage(MemoryPort::default(), PLANS),
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
    let service = CollaborationService::with_storage(port.clone(), PLANS);
    let document = note();
    service
        .bootstrap(
            &NOTE_PLAN,
            &document,
            RELATIVE_PATH,
            &seed_document(),
            accept,
        )
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
fn a_publication_scan_reads_bytes_without_asking_for_versions() {
    let port = MemoryPort::default();
    let service = CollaborationService::with_storage(port.clone(), PLANS);
    let document = note();
    service
        .bootstrap(
            &NOTE_PLAN,
            &document,
            RELATIVE_PATH,
            &seed_document(),
            accept,
        )
        .expect("seed");
    port.state().versioned_reads = 0;

    let scan = service.publication_scan().expect("scan");

    assert!(scan
        .publications
        .iter()
        .any(|publication| publication.document == document));
    assert_eq!(port.state().versioned_reads, 0);
}

#[test]
fn a_rescan_reads_again_only_the_envelopes_whose_bytes_changed() {
    let port = MemoryPort::default();
    let service = CollaborationService::with_storage(port.clone(), PLANS);
    let edits = edits(1);
    let document = note();
    service
        .bootstrap_update(
            &NOTE_PLAN,
            &document,
            RELATIVE_PATH,
            NOTE_PLAN.schema_version,
            &edits.seed_update,
            accept,
        )
        .expect("seed");
    let neighbour = CollaborationDocumentId::new("Note", "note-2");
    service
        .bootstrap(
            &NOTE_PLAN,
            &neighbour,
            "notes/note-2.yaml",
            &json!({ "note_id": "note-2", "body": "Seeded.", "etag": "" }),
            accept,
        )
        .expect("seed neighbour");
    let mut cache = CollaborationPublicationScanCache::default();
    let first = service.publication_rescan(&mut cache).expect("first scan");

    port.state().byte_reads = 0;
    let unchanged = service
        .publication_rescan(&mut cache)
        .expect("unchanged scan");
    assert_eq!(unchanged.publications, first.publications);
    assert_eq!(port.state().byte_reads, 0);

    let (operation_id, base, update) = &edits.edits[0];
    let imported = service
        .import(&NOTE_PLAN, request(operation_id, base, update), accept)
        .expect("commit");
    port.state().byte_reads = 0;
    let changed = service
        .publication_rescan(&mut cache)
        .expect("changed scan");
    assert_eq!(port.state().byte_reads, 1);
    let edited = changed
        .publications
        .iter()
        .find(|publication| publication.document == document)
        .expect("the edited document is published");
    assert_eq!(edited.generation, imported.generation);
    assert!(changed
        .publications
        .iter()
        .any(|publication| publication.document == neighbour));
}

#[test]
fn a_rescan_drops_an_envelope_that_is_no_longer_kept() {
    let port = MemoryPort::default();
    let service = CollaborationService::with_storage(port.clone(), PLANS);
    let document = note();
    service
        .bootstrap(
            &NOTE_PLAN,
            &document,
            RELATIVE_PATH,
            &seed_document(),
            accept,
        )
        .expect("seed");
    let mut cache = CollaborationPublicationScanCache::default();
    service.publication_rescan(&mut cache).expect("first scan");

    let source = port.source(&document);
    port.state().envelopes.remove(&source);
    let scan = service.publication_rescan(&mut cache).expect("rescan");

    assert!(scan
        .publications
        .iter()
        .all(|publication| publication.document != document));
}

#[test]
fn commits_racing_through_one_port_both_land() {
    let port = MemoryPort::default();
    let first = CollaborationService::with_storage(port.clone(), PLANS);
    let second = CollaborationService::with_storage(port, PLANS);
    let document = note();
    first
        .bootstrap(
            &NOTE_PLAN,
            &document,
            RELATIVE_PATH,
            &seed_document(),
            accept,
        )
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
    let service = CollaborationService::with_storage(port.clone(), PLANS);
    let document = note();
    service
        .bootstrap_update(
            &NOTE_PLAN,
            &document,
            RELATIVE_PATH,
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
        .move_document(&document, &renamed, "notes/note-2.yaml")
        .expect("move")
        .expect("the document exists");

    assert!(
        service.load(&document).unwrap().is_none(),
        "the source is gone"
    );
    let moved = service.load(&renamed).unwrap().expect("moved");
    assert_eq!(moved.pending_rename_from.as_deref(), Some("note-1"));
    assert!(moved
        .retained_operations
        .iter()
        .any(|operation| operation.operation_id == edits.edits[0].0));
    assert_eq!(
        service.detail(&NOTE_PLAN, &renamed).unwrap().unwrap()["body"],
        json!("Edit 0.")
    );
}

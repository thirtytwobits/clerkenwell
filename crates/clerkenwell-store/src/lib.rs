//! Durable single-authority storage for collaborative documents.
//!
//! One envelope per document holds a checksummed checkpoint and a window of
//! retained operations. A commit imports an update into the accepted replica,
//! judges it against the plan's field policies, validates the materialised
//! document, and replaces the envelope only if it is still the one the commit
//! read. Publication is acknowledged separately from the commit. Corruption is
//! reported, never read as an empty document, and recovery preserves evidence
//! and records an audit trail.
//!
//! The service builds and judges every envelope. A storage port keeps each
//! document's envelope as bytes and replaces or removes them only from the
//! version a writer read; the local file port does so under a per-document
//! lock with an atomic durable write.

mod error;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clerkenwell_doc::{CollaborationLoroError, LoroAuthoringDocument};
use clerkenwell_schema::GeneratedCollaborationEntitySpec;
pub use error::{StoreError, StoreErrorKind, StoreResult};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The envelope format this store reads and writes.
pub const ENVELOPE_VERSION: u32 = 1;
const RETAINED_OPERATION_COUNT: usize = 8;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CollaborationFaultPoint {
    CandidateImported,
    Materialised,
    Validated,
    BeforeTemporaryWrite,
    AfterTemporarySync,
    AfterRenameBeforeDirectorySync,
    AfterDurableCommit,
    BeforePublicationAcknowledgement,
}

/// One pending injected fault, shared by a service and its local storage so a
/// test can interrupt a commit at any boundary, whichever of them owns it.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct CollaborationFaults(Arc<Mutex<Option<CollaborationFaultPoint>>>);

#[cfg(test)]
impl CollaborationFaults {
    fn inject(&self, point: CollaborationFaultPoint) {
        *self.0.lock().expect("collaboration fault lock") = Some(point);
    }

    fn fail_if(&self, point: CollaborationFaultPoint) -> StoreResult<()> {
        let mut pending = self.0.lock().expect("collaboration fault lock");
        if pending.as_ref() == Some(&point) {
            pending.take();
            return Err(StoreError::internal(format!(
                "Injected collaboration fault at {point:?}."
            ))
            .with_data(serde_json::json!({
                "code": "collaboration_fault_injected",
                "boundary": format!("{point:?}"),
            })));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollaborationDocumentId {
    pub entity: String,
    pub resource_id: String,
}

impl CollaborationDocumentId {
    pub fn new(entity: impl Into<String>, resource_id: impl Into<String>) -> Self {
        Self {
            entity: entity.into(),
            resource_id: resource_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationOperation {
    pub operation_id: String,
    pub sequence: u64,
    pub schema_version: u32,
    pub update_base64: String,
    pub update_sha256: String,
    pub update_bytes: usize,
}

impl CollaborationOperation {
    pub fn from_update(
        operation_id: String,
        sequence: u64,
        schema_version: u32,
        update: &[u8],
    ) -> Self {
        let update_sha256 = sha256_hex(update);
        Self {
            operation_id,
            sequence,
            schema_version,
            update_base64: BASE64.encode(update),
            update_sha256,
            update_bytes: update.len(),
        }
    }

    pub fn decode(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        let update = BASE64
            .decode(self.update_base64.as_bytes())
            .map_err(|error| {
                corrupt_state(
                    document,
                    format!(
                        "operation {} is not valid base64: {error}",
                        self.operation_id
                    ),
                )
            })?;
        if update.len() != self.update_bytes || sha256_hex(&update) != self.update_sha256 {
            return Err(corrupt_state(
                document,
                format!("operation {} failed checksum validation", self.operation_id),
            ));
        }
        Ok(update)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableCollaborationEnvelope {
    pub envelope_version: u32,
    pub entity: String,
    pub resource_id: String,
    pub relative_path: String,
    pub schema_version: u32,
    pub generation: u64,
    pub checkpoint_sequence: u64,
    pub compacted_through_sequence: u64,
    pub checkpoint_update_base64: String,
    pub checkpoint_sha256: String,
    pub checkpoint_bytes: usize,
    pub retained_operations: Vec<CollaborationOperation>,
    pub publication_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_rename_from: Option<String>,
}

impl DurableCollaborationEnvelope {
    pub fn checkpoint_update(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        let update = BASE64
            .decode(self.checkpoint_update_base64.as_bytes())
            .map_err(|error| {
                corrupt_state(document, format!("checkpoint is not valid base64: {error}"))
            })?;
        if update.len() != self.checkpoint_bytes || sha256_hex(&update) != self.checkpoint_sha256 {
            return Err(corrupt_state(
                document,
                "checkpoint failed checksum validation",
            ));
        }
        Ok(update)
    }

    pub fn projection_revision_floor(&self) -> u64 {
        self.checkpoint_sequence.saturating_add(1).max(2)
    }
}

/// One operation to commit over the envelope the service read.
#[derive(Debug, Clone)]
struct CollaborationCommit {
    document: CollaborationDocumentId,
    relative_path: String,
    schema_version: u32,
    operation_id: String,
    imported_update: Vec<u8>,
    has_new_operations: bool,
    accepted_update: Vec<u8>,
}

#[derive(Debug, Clone)]
enum CollaborationCommitOutcome {
    Accepted(DurableCollaborationEnvelope),
    Duplicate(DurableCollaborationEnvelope),
    /// Another writer replaced the envelope after it was read.
    Stale,
}

/// An envelope as the service read it, with the version that names it.
#[derive(Debug, Clone)]
struct ReadEnvelope {
    version: String,
    envelope: DurableCollaborationEnvelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationExchangeMode {
    Incremental,
    Bootstrap,
}

#[derive(Debug, Clone)]
pub struct CollaborationAuthoringState {
    pub schema_version: u32,
    pub accepted_frontier_base64: String,
    pub update_base64: String,
    pub etag: String,
}

#[derive(Debug, Clone)]
pub struct CollaborationImportRequest {
    pub document: CollaborationDocumentId,
    pub relative_path: String,
    pub schema_version: u32,
    pub operation_id: String,
    pub exchange_mode: CollaborationExchangeMode,
    pub base_frontier_base64: String,
    pub update_base64: String,
    pub fence: ImportFence,
}

/// What an import is judged against when it commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportFence {
    /// The accepted document must still be the one read at this etag.
    Etag(String),
    /// Operations after the base frontier merge under each field's conflict
    /// policy.
    Frontier,
}

#[derive(Debug, Clone)]
pub struct CollaborationImportResult {
    pub operation_id: String,
    pub duplicate: bool,
    pub schema_version: u32,
    pub accepted_frontier_base64: String,
    pub update_base64: String,
    pub etag: String,
    pub materialized: Value,
    pub generation: u64,
    pub peer_id: String,
    pub oplog_version: String,
    pub state_frontiers: String,
    pub update_bytes: usize,
}

impl CollaborationImportResult {
    /// The accepted state this import left, as a fresh read would report it.
    pub fn authoring_state(&self) -> CollaborationAuthoringState {
        CollaborationAuthoringState {
            schema_version: self.schema_version,
            accepted_frontier_base64: self.accepted_frontier_base64.clone(),
            update_base64: self.update_base64.clone(),
            etag: self.etag.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationPublication {
    pub document: CollaborationDocumentId,
    pub schema_version: u32,
    pub generation: u64,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationPublicationScanProblem {
    pub source: String,
    pub fingerprint: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct CollaborationPublicationScan {
    pub publications: Vec<CollaborationPublication>,
    pub problems: Vec<CollaborationPublicationScanProblem>,
}

#[derive(Debug, Deserialize)]
struct DurableCollaborationPublicationHeader {
    envelope_version: u32,
    entity: String,
    resource_id: String,
    schema_version: u32,
    generation: u64,
    publication_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CollaborationDocumentInspection {
    pub entity: String,
    pub resource_id: String,
    pub schema_version: u32,
    pub generation: u64,
    pub checkpoint_sequence: u64,
    pub compacted_through_sequence: u64,
    pub retained_operation_count: usize,
    pub checkpoint_bytes: usize,
    pub retained_operation_bytes: usize,
    pub publication_pending: bool,
    pub migration_required: bool,
    pub valid: bool,
    pub failure_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationRecoveryAction {
    Export,
    Quarantine,
    Repair,
    Reindex,
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollaborationRecoveryAuditRecord {
    pub timestamp_unix_ms: u128,
    pub action: CollaborationRecoveryAction,
    pub entity: String,
    pub resource_id: String,
    pub reason_sha256: Option<String>,
    pub destructive: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CollaborationRuntimeCountersSnapshot {
    pub duplicate_imports: u64,
    pub dependency_blocks: u64,
    pub resync_requirements: u64,
    pub materialisation_failures: u64,
    pub commit_contention: u64,
}

#[derive(Debug, Default)]
struct CollaborationRuntimeCounters {
    duplicate_imports: AtomicU64,
    dependency_blocks: AtomicU64,
    resync_requirements: AtomicU64,
    materialisation_failures: AtomicU64,
    commit_contention: AtomicU64,
}

impl CollaborationRuntimeCounters {
    fn snapshot(&self) -> CollaborationRuntimeCountersSnapshot {
        CollaborationRuntimeCountersSnapshot {
            duplicate_imports: self.duplicate_imports.load(Ordering::Relaxed),
            dependency_blocks: self.dependency_blocks.load(Ordering::Relaxed),
            resync_requirements: self.resync_requirements.load(Ordering::Relaxed),
            materialisation_failures: self.materialisation_failures.load(Ordering::Relaxed),
            commit_contention: self.commit_contention.load(Ordering::Relaxed),
        }
    }
}

/// A stored envelope's bytes and the version a conditional write names them
/// by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEnvelope {
    pub version: String,
    pub bytes: Vec<u8>,
}

/// Where envelopes are kept. The service reads, builds and judges every
/// envelope; a port keeps each document's bytes and replaces or removes them
/// only from the version the writer read.
pub trait CollaborationStoragePort: std::fmt::Debug + Send + Sync {
    /// Where `document`'s envelope is kept, named as `sources` names it.
    fn source(&self, document: &CollaborationDocumentId) -> String;

    /// Where each stored envelope of `entity` is kept, in a stable order.
    fn sources(&self, entity: &str) -> StoreResult<Vec<String>>;

    /// The envelope kept at `source`, or `None` when none is.
    fn read(&self, source: &str) -> StoreResult<Option<StoredEnvelope>>;

    /// Keeps `bytes` as `document`'s envelope if its stored version is still
    /// `expected` (`None`: nothing is stored), and returns their version.
    /// Returns `None` and changes nothing when the stored version has moved.
    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>>;

    /// Removes `document`'s envelope if its stored version is still
    /// `expected`. Returns false and changes nothing when it has moved.
    fn compare_and_remove(
        &self,
        document: &CollaborationDocumentId,
        expected: &str,
    ) -> StoreResult<bool>;

    /// Keeps `bytes`, read as `document`'s envelope, as evidence under
    /// `label`, and returns where.
    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<PathBuf>;

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()>;

    /// Every recorded recovery request, oldest first.
    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>>;
}

/// Entity-neutral collaboration transaction service.
///
/// Generated plans define document layout. Resource adapters provide only
/// their seed document and semantic validator.
#[derive(Debug, Clone)]
pub struct CollaborationService<S = LocalFileCollaborationStorage> {
    storage: S,
    plans: &'static [GeneratedCollaborationEntitySpec],
    counters: Arc<CollaborationRuntimeCounters>,
    #[cfg(test)]
    faults: CollaborationFaults,
}

impl CollaborationService<LocalFileCollaborationStorage> {
    /// A service over the same plans, sharing this one's counters, whose
    /// envelopes live under `root`.
    pub fn with_storage_root(&self, root: &Path) -> Self {
        let storage = LocalFileCollaborationStorage::new(root);
        Self {
            #[cfg(test)]
            faults: storage.faults.clone(),
            storage,
            plans: self.plans,
            counters: self.counters.clone(),
        }
    }

    /// A service for documents of `plans` whose envelopes live under `root`.
    pub fn new(root: &Path, plans: &'static [GeneratedCollaborationEntitySpec]) -> Self {
        let storage = LocalFileCollaborationStorage::new(root);
        Self {
            #[cfg(test)]
            faults: storage.faults.clone(),
            storage,
            plans,
            counters: Arc::new(CollaborationRuntimeCounters::default()),
        }
    }
}

impl<S: CollaborationStoragePort> CollaborationService<S> {
    /// A service for documents of `plans` committing through any
    /// implementation of the storage port.
    pub fn with_storage(storage: S, plans: &'static [GeneratedCollaborationEntitySpec]) -> Self {
        Self {
            storage,
            plans,
            counters: Arc::new(CollaborationRuntimeCounters::default()),
            #[cfg(test)]
            faults: CollaborationFaults::default(),
        }
    }

    pub fn storage(&self) -> &S {
        &self.storage
    }

    /// `document`'s validated envelope, or `None` when it has none.
    pub fn load(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<DurableCollaborationEnvelope>> {
        Ok(self.read_envelope(document)?.map(|read| read.envelope))
    }

    /// Every stored envelope of `entity`, validated, in resource order.
    pub fn envelopes(&self, entity: &str) -> StoreResult<Vec<DurableCollaborationEnvelope>> {
        let mut envelopes = Vec::new();
        for source in self.storage.sources(entity)? {
            let Some(stored) = self.storage.read(&source)? else {
                continue;
            };
            let envelope: DurableCollaborationEnvelope = serde_json::from_slice(&stored.bytes)
                .map_err(|error| {
                    StoreError::internal(format!(
                        "Collaboration envelope {source} is invalid JSON: {error}"
                    ))
                })?;
            let document =
                CollaborationDocumentId::new(envelope.entity.clone(), envelope.resource_id.clone());
            validate_envelope(&document, &envelope)?;
            envelopes.push(envelope);
        }
        envelopes.sort_by(|left, right| left.resource_id.cmp(&right.resource_id));
        Ok(envelopes)
    }

    /// Stores `envelope` for a document that has none, and returns the
    /// document's envelope either way.
    pub fn install(
        &self,
        envelope: DurableCollaborationEnvelope,
    ) -> StoreResult<DurableCollaborationEnvelope> {
        let document =
            CollaborationDocumentId::new(envelope.entity.clone(), envelope.resource_id.clone());
        loop {
            if let Some(current) = self.load(&document)? {
                return Ok(current);
            }
            if self.swap(&document, None, &envelope)?.is_some() {
                return Ok(envelope);
            }
        }
    }

    fn read_envelope(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<ReadEnvelope>> {
        let source = self.storage.source(document);
        let Some(stored) = self.storage.read(&source)? else {
            return Ok(None);
        };
        let envelope: DurableCollaborationEnvelope = serde_json::from_slice(&stored.bytes)
            .map_err(|error| {
                corrupt_state(
                    document,
                    format!("envelope {source} is invalid JSON: {error}"),
                )
            })?;
        validate_envelope(document, &envelope)?;
        Ok(Some(ReadEnvelope {
            version: stored.version,
            envelope,
        }))
    }

    /// Stores `envelope` as `document`'s if the stored version is still
    /// `expected`, returning the new version, or `None` when it has moved.
    fn swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        envelope: &DurableCollaborationEnvelope,
    ) -> StoreResult<Option<String>> {
        validate_envelope(document, envelope)?;
        let bytes = serde_json::to_vec_pretty(envelope)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        self.storage.compare_and_swap(document, expected, &bytes)
    }

    /// Appends `commit`'s operation to the envelope the service read, or
    /// reports it as a duplicate of one that envelope already holds.
    fn commit(
        &self,
        current: Option<&ReadEnvelope>,
        commit: CollaborationCommit,
    ) -> StoreResult<CollaborationCommitOutcome> {
        let document = commit.document;
        let operation_id = commit.operation_id;
        if let Some(ReadEnvelope { envelope, .. }) = current {
            if let Some(existing) = envelope
                .retained_operations
                .iter()
                .find(|operation| operation.operation_id == operation_id)
            {
                if existing.update_sha256 == sha256_hex(&commit.imported_update) {
                    return Ok(CollaborationCommitOutcome::Duplicate(envelope.clone()));
                }
                return Err(StoreError::invalid_request(format!(
                    "Collaboration operation id {operation_id:?} was reused with different content."
                ))
                .with_data(serde_json::json!({
                    "code": "collaboration_operation_id_collision",
                    "entity": document.entity,
                    "resource_id": document.resource_id,
                    "operation_id": operation_id,
                })));
            }
            if !commit.has_new_operations
                || envelope.checkpoint_sha256 == sha256_hex(&commit.accepted_update)
            {
                return Ok(CollaborationCommitOutcome::Duplicate(envelope.clone()));
            }
        }
        let current_envelope = current.map(|read| &read.envelope);
        let sequence =
            current_envelope.map_or(1, |state| state.checkpoint_sequence.saturating_add(1));
        let schema_changed =
            current_envelope.is_some_and(|state| state.schema_version != commit.schema_version);
        let mut retained_operations = if schema_changed {
            Vec::new()
        } else {
            current_envelope.map_or_else(Vec::new, |state| state.retained_operations.clone())
        };
        retained_operations.push(CollaborationOperation::from_update(
            operation_id,
            sequence,
            commit.schema_version,
            &commit.imported_update,
        ));
        if retained_operations.len() > RETAINED_OPERATION_COUNT {
            let remove_count = retained_operations.len() - RETAINED_OPERATION_COUNT;
            retained_operations.drain(0..remove_count);
        }
        let retained_from = retained_operations
            .first()
            .map_or(sequence, |operation| operation.sequence);
        let envelope = DurableCollaborationEnvelope {
            envelope_version: ENVELOPE_VERSION,
            entity: document.entity.clone(),
            resource_id: document.resource_id.clone(),
            relative_path: commit.relative_path,
            schema_version: commit.schema_version,
            generation: current_envelope
                .map_or(0, |state| state.generation)
                .saturating_add(1),
            checkpoint_sequence: sequence,
            compacted_through_sequence: retained_from.saturating_sub(1),
            checkpoint_update_base64: BASE64.encode(&commit.accepted_update),
            checkpoint_sha256: sha256_hex(&commit.accepted_update),
            checkpoint_bytes: commit.accepted_update.len(),
            retained_operations,
            publication_pending: true,
            pending_rename_from: None,
        };
        let expected = current.map(|read| read.version.as_str());
        Ok(match self.swap(&document, expected, &envelope)? {
            Some(_) => CollaborationCommitOutcome::Accepted(envelope),
            None => CollaborationCommitOutcome::Stale,
        })
    }

    pub fn detail(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<Value>> {
        let Some(envelope) = self.load(document)? else {
            return Ok(None);
        };
        let authoring = load_authoring_document(plan, document, &envelope)?;
        let etag = collaboration_etag(&envelope.checkpoint_update(document)?);
        authoring
            .materialized_document(&etag)
            .map(Some)
            .map_err(|error| collaboration_loro_error(document, error))
    }

    pub fn authoring_state(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
    ) -> StoreResult<CollaborationAuthoringState> {
        let envelope = self.load(document)?.ok_or_else(|| {
            StoreError::not_found(format!(
                "No collaborative {} document exists for \"{}\".",
                document.entity, document.resource_id
            ))
            .with_data(serde_json::json!({
                "code": "collaboration_document_not_found",
                "entity": document.entity,
                "resource_id": document.resource_id,
            }))
        })?;
        let update = envelope.checkpoint_update(document)?;
        let authoring = load_authoring_document(plan, document, &envelope)?;
        Ok(CollaborationAuthoringState {
            schema_version: envelope.schema_version,
            accepted_frontier_base64: authoring.accepted_frontier_base64(),
            update_base64: BASE64.encode(&update),
            etag: collaboration_etag(&update),
        })
    }

    pub fn import<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        request: CollaborationImportRequest,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<CollaborationImportResult, E> {
        self.import_internal(plan, request, validate, true)
    }

    /// Commits a lifecycle rewrite — an operation produced by the engine
    /// itself, serialised under the resource store lock — without the
    /// concurrent-edit field policy applied to authoring imports. The one
    /// caller is rename, which must rewrite the immutable identity field so
    /// the materialised document matches its moved envelope.
    pub fn import_lifecycle_rewrite<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        request: CollaborationImportRequest,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<CollaborationImportResult, E> {
        self.import_internal(plan, request, validate, false)
    }

    fn import_internal<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        request: CollaborationImportRequest,
        validate: impl Fn(&Value) -> Result<(), E>,
        enforce_field_policy: bool,
    ) -> Result<CollaborationImportResult, E> {
        if request.schema_version != plan.schema_version {
            return Err(StoreError::invalid_request(format!(
                "Unsupported {} collaboration schema version {}; expected {}.",
                request.document.entity, request.schema_version, plan.schema_version
            ))
            .with_data(serde_json::json!({
                "code": "unsupported_collaboration_schema_version",
                "entity": request.document.entity,
                "resource_id": request.document.resource_id,
                "actual": request.schema_version,
                "expected": plan.schema_version,
            }))
            .into());
        }
        if request.document.resource_id.trim().is_empty() {
            return Err(StoreError::invalid_request(
                "A collaboration resource identity must not be empty.",
            )
            .into());
        }
        if request.operation_id.trim().is_empty() {
            return Err(StoreError::invalid_request(
                "A collaboration import requires a stable non-empty operation_id.",
            )
            .with_data(serde_json::json!({
                "code": "invalid_collaboration_operation_id",
                "entity": request.document.entity,
                "resource_id": request.document.resource_id,
            }))
            .into());
        }
        let imported_update = BASE64
            .decode(request.update_base64.as_bytes())
            .map_err(|error| {
                StoreError::invalid_request(format!("Invalid Loro update_base64: {error}"))
                    .with_data(serde_json::json!({
                        "code": "invalid_collaboration_update",
                        "entity": request.document.entity,
                        "resource_id": request.document.resource_id,
                    }))
            })?;

        for _attempt in 0..8 {
            let read = self.read_envelope(&request.document)?;
            let current = match read.as_ref().map(|read| &read.envelope) {
                Some(_) if request.exchange_mode == CollaborationExchangeMode::Bootstrap => {
                    return Err(StoreError::conflict(format!(
                        "Collaborative {} document \"{}\" already exists.",
                        request.document.entity, request.document.resource_id
                    ))
                    .with_data(serde_json::json!({
                        "code": "collaboration_document_exists",
                        "entity": request.document.entity,
                        "resource_id": request.document.resource_id,
                        "exchange_mode": "bootstrap",
                        "draft_retained": true,
                    }))
                    .into())
                }
                Some(current) => current.clone(),
                None if request.exchange_mode == CollaborationExchangeMode::Bootstrap => {
                    let authoring = LoroAuthoringDocument::from_versioned_update_base64(
                        plan,
                        request.schema_version,
                        &request.update_base64,
                    )
                    .map_err(|error| {
                        self.record_loro_failure(&error);
                        collaboration_loro_error(&request.document, error)
                    })?;
                    let accepted_update =
                        BASE64
                            .decode(authoring.export_update_base64().map_err(|error| {
                                collaboration_loro_error(&request.document, error)
                            })?)
                            .map_err(|error| StoreError::internal(error.to_string()))?;
                    DurableCollaborationEnvelope {
                        envelope_version: ENVELOPE_VERSION,
                        entity: request.document.entity.clone(),
                        resource_id: request.document.resource_id.clone(),
                        relative_path: request.relative_path.clone(),
                        schema_version: request.schema_version,
                        generation: 0,
                        checkpoint_sequence: 0,
                        compacted_through_sequence: 0,
                        checkpoint_update_base64: BASE64.encode(&accepted_update),
                        checkpoint_sha256: sha256_hex(&accepted_update),
                        checkpoint_bytes: accepted_update.len(),
                        retained_operations: Vec::new(),
                        publication_pending: false,
                        pending_rename_from: None,
                    }
                }
                None => {
                    return Err(StoreError::not_found(format!(
                        "No collaborative {} document exists for \"{}\".",
                        request.document.entity, request.document.resource_id
                    ))
                    .with_data(serde_json::json!({
                        "code": "collaboration_document_not_found",
                        "entity": request.document.entity,
                        "resource_id": request.document.resource_id,
                        "exchange_mode": "incremental",
                    }))
                    .into())
                }
            };
            let authoring = load_authoring_document(plan, &request.document, &current)?;
            let current_update = current.checkpoint_update(&request.document)?;
            let current_etag = collaboration_etag(&current_update);
            // An operation already in the retained window was accepted: its
            // retry is a duplicate, however far the document has moved since.
            let already_accepted = current
                .retained_operations
                .iter()
                .any(|operation| operation.operation_id == request.operation_id);
            if let ImportFence::Etag(expected_etag) = &request.fence {
                if !already_accepted && *expected_etag != current_etag {
                    let current_document = authoring
                        .materialized_document(&current_etag)
                        .map_err(|error| collaboration_loro_error(&request.document, error))?;
                    let mut data = serde_json::json!({
                        "code": "conflict",
                        "conflict_kind": "collaboration_revision",
                        "entity": request.document.entity,
                        "resource_id": request.document.resource_id,
                        "current": current_document,
                        "current_etag": current_etag,
                        "expected_etag": expected_etag,
                        "draft_retained": true,
                    });
                    // A caller names the document by its plan's identity field.
                    data[plan.id_field] = Value::from(request.document.resource_id.clone());
                    return Err(StoreError::conflict(format!(
                        "{} \"{}\" has advanced since the caller read it.",
                        request.document.entity, request.document.resource_id
                    ))
                    .with_data(data)
                    .into());
                }
            }
            if request.exchange_mode == CollaborationExchangeMode::Incremental {
                authoring
                    .require_frontier_base64(&request.base_frontier_base64)
                    .map_err(|error| {
                        self.record_loro_failure(&error);
                        collaboration_loro_error(&request.document, error)
                    })?;
            }
            if enforce_field_policy
                && request.exchange_mode == CollaborationExchangeMode::Incremental
            {
                let conflicts = authoring
                    .policy_conflicts_for_incremental_update(
                        &request.base_frontier_base64,
                        request.schema_version,
                        &request.update_base64,
                    )
                    .map_err(|error| {
                        self.record_loro_failure(&error);
                        collaboration_loro_error(&request.document, error)
                    })?;
                if !conflicts.is_empty() {
                    let current_document = authoring
                        .materialized_document(&current_etag)
                        .map_err(|error| collaboration_loro_error(&request.document, error))?;
                    return Err(StoreError::conflict(format!(
                        "Concurrent {} edits require explicit resolution for: {}.",
                        request.document.entity,
                        conflicts.join(", ")
                    ))
                    .with_data(serde_json::json!({
                        "code": "conflict",
                        "conflict_kind": "collaboration_policy",
                        "entity": request.document.entity,
                        "resource_id": request.document.resource_id,
                        "conflict_paths": conflicts,
                        "current": current_document,
                        "current_etag": current_etag,
                        "base_frontier_base64": request.base_frontier_base64,
                        "draft_retained": true,
                    }))
                    .into());
                }
            }
            let before_import = authoring.accepted_frontier_base64();
            authoring
                .import_versioned_update_base64(request.schema_version, &request.update_base64)
                .map_err(|error| {
                    self.record_loro_failure(&error);
                    collaboration_loro_error(&request.document, error)
                })?;
            #[cfg(test)]
            self.faults
                .fail_if(CollaborationFaultPoint::CandidateImported)?;
            let accepted_update = BASE64
                .decode(
                    authoring
                        .export_update_base64()
                        .map_err(|error| collaboration_loro_error(&request.document, error))?,
                )
                .map_err(|error| StoreError::internal(error.to_string()))?;
            let etag = collaboration_etag(&accepted_update);
            let materialized = authoring.materialized_document(&etag).map_err(|error| {
                self.counters
                    .materialisation_failures
                    .fetch_add(1, Ordering::Relaxed);
                collaboration_loro_error(&request.document, error)
            })?;
            #[cfg(test)]
            self.faults.fail_if(CollaborationFaultPoint::Materialised)?;
            validate(&materialized)?;
            #[cfg(test)]
            self.faults.fail_if(CollaborationFaultPoint::Validated)?;
            let outcome = self.commit(
                read.as_ref(),
                CollaborationCommit {
                    document: request.document.clone(),
                    relative_path: request.relative_path.clone(),
                    schema_version: request.schema_version,
                    operation_id: request.operation_id.clone(),
                    imported_update: imported_update.clone(),
                    has_new_operations: before_import != authoring.accepted_frontier_base64(),
                    accepted_update: accepted_update.clone(),
                },
            )?;
            let (accepted, duplicate) = match outcome {
                CollaborationCommitOutcome::Stale => continue,
                CollaborationCommitOutcome::Accepted(accepted) => (accepted, false),
                CollaborationCommitOutcome::Duplicate(accepted) => (accepted, true),
            };
            #[cfg(test)]
            self.faults
                .fail_if(CollaborationFaultPoint::AfterDurableCommit)?;
            if duplicate {
                self.counters
                    .duplicate_imports
                    .fetch_add(1, Ordering::Relaxed);
            }
            // Return the durable checkpoint, including on retries. Exporting the
            // same history again need not reproduce its original snapshot bytes.
            let durable_update = accepted.checkpoint_update(&request.document)?;
            let (authoring, materialized) = if durable_update == accepted_update {
                (authoring, materialized)
            } else {
                let durable = load_authoring_document(plan, &request.document, &accepted)?;
                let value = durable
                    .materialized_document(&collaboration_etag(&durable_update))
                    .map_err(|error| collaboration_loro_error(&request.document, error))?;
                (durable, value)
            };
            let accepted_update = durable_update;
            let debug = authoring.debug();
            return Ok(CollaborationImportResult {
                operation_id: request.operation_id,
                duplicate,
                schema_version: accepted.schema_version,
                accepted_frontier_base64: authoring.accepted_frontier_base64(),
                etag: collaboration_etag(&accepted_update),
                update_base64: BASE64.encode(&accepted_update),
                materialized,
                generation: accepted.generation,
                peer_id: debug.peer_id,
                oplog_version: debug.oplog_version,
                state_frontiers: debug.state_frontiers,
                update_bytes: accepted_update.len(),
            });
        }
        self.counters
            .commit_contention
            .fetch_add(1, Ordering::Relaxed);
        Err(StoreError::conflict(format!(
            "Collaboration document {}/{} changed repeatedly while committing; the draft was retained.",
            request.document.entity, request.document.resource_id
        ))
        .with_data(serde_json::json!({
            "code": "collaboration_commit_contended",
            "entity": request.document.entity,
            "resource_id": request.document.resource_id,
            "draft_retained": true,
        })).into())
    }

    pub fn acknowledge_publication(
        &self,
        document: &CollaborationDocumentId,
        generation: u64,
    ) -> StoreResult<()> {
        #[cfg(test)]
        self.faults
            .fail_if(CollaborationFaultPoint::BeforePublicationAcknowledgement)?;
        loop {
            let Some(read) = self.read_envelope(document)? else {
                return Err(missing_state(document));
            };
            if read.envelope.generation != generation || !read.envelope.publication_pending {
                return Ok(());
            }
            let mut envelope = read.envelope;
            envelope.publication_pending = false;
            if self
                .swap(document, Some(&read.version), &envelope)?
                .is_some()
            {
                return Ok(());
            }
        }
    }

    pub fn publication_scan(&self) -> StoreResult<CollaborationPublicationScan> {
        let mut scan = CollaborationPublicationScan::default();
        for spec in self.plans {
            let entity_scan = self.entity_publications(spec.name)?;
            scan.publications.extend(entity_scan.publications);
            scan.problems.extend(entity_scan.problems);
        }
        scan.publications.sort_by(|left, right| {
            (&left.document.entity, &left.document.resource_id)
                .cmp(&(&right.document.entity, &right.document.resource_id))
        });
        scan.problems
            .sort_by(|left, right| left.source.cmp(&right.source));
        Ok(scan)
    }

    /// Reads each stored envelope of `entity` only as far as its identity
    /// and publication state, reporting one that cannot be read that far
    /// rather than failing the scan.
    fn entity_publications(&self, entity: &str) -> StoreResult<CollaborationPublicationScan> {
        let mut scan = CollaborationPublicationScan::default();
        for source in self.storage.sources(entity)? {
            let bytes = match self.storage.read(&source) {
                Ok(Some(stored)) => stored.bytes,
                Ok(None) => continue,
                Err(error) => {
                    let message =
                        format!("Could not read collaboration publication {source}: {error}");
                    scan.problems.push(CollaborationPublicationScanProblem {
                        source,
                        fingerprint: sha256_hex(message.as_bytes()),
                        code: "collaboration_publication_unreadable",
                        message,
                    });
                    continue;
                }
            };
            let header: DurableCollaborationPublicationHeader = match serde_json::from_slice(&bytes)
            {
                Ok(header) => header,
                Err(error) => {
                    scan.problems.push(CollaborationPublicationScanProblem {
                        message: format!(
                            "Collaboration publication {source} is invalid JSON: {error}"
                        ),
                        source,
                        fingerprint: sha256_hex(&bytes),
                        code: "collaboration_publication_invalid_json",
                    });
                    continue;
                }
            };
            let document =
                CollaborationDocumentId::new(header.entity.clone(), header.resource_id.clone());
            if header.envelope_version != ENVELOPE_VERSION
                || header.entity != entity
                || self.storage.source(&document) != source
            {
                scan.problems.push(CollaborationPublicationScanProblem {
                    message: format!(
                        "Collaboration publication header in {source} does not match its storage identity"
                    ),
                    source,
                    fingerprint: sha256_hex(&bytes),
                    code: "collaboration_state_corrupt",
                });
                continue;
            }
            scan.publications.push(CollaborationPublication {
                document,
                schema_version: header.schema_version,
                generation: header.generation,
                pending: header.publication_pending,
            });
        }
        Ok(scan)
    }

    #[cfg(test)]
    pub fn publications(&self) -> StoreResult<Vec<CollaborationPublication>> {
        let scan = self.publication_scan()?;
        if let Some(problem) = scan.problems.first() {
            return Err(StoreError::internal(problem.message.clone()).with_data(
                serde_json::json!({
                    "code": problem.code,
                    "source": problem.source,
                    "fingerprint": problem.fingerprint,
                }),
            ));
        }
        Ok(scan.publications)
    }

    /// Removes `document`'s envelope, whatever it holds.
    pub fn delete(&self, document: &CollaborationDocumentId) -> StoreResult<()> {
        let source = self.storage.source(document);
        while let Some(stored) = self.storage.read(&source)? {
            if self.storage.compare_and_remove(document, &stored.version)? {
                break;
            }
        }
        Ok(())
    }

    /// Moves `source`'s envelope to `destination`, recording where it came
    /// from so an interrupted rename can be finished. Returns the moved
    /// envelope's generation, or `None` when `source` has none.
    pub fn move_document(
        &self,
        source: &CollaborationDocumentId,
        destination: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>> {
        if source.entity != destination.entity {
            return Err(StoreError::invalid_request(
                "A collaboration document cannot move between entities.",
            ));
        }
        loop {
            let Some(read) = self.read_envelope(source)? else {
                return Ok(None);
            };
            if self
                .storage
                .read(&self.storage.source(destination))?
                .is_some()
            {
                return Err(StoreError::conflict(format!(
                    "Collaboration state already exists for {}/{}.",
                    destination.entity, destination.resource_id
                ))
                .with_data(serde_json::json!({
                    "code": "collaboration_destination_exists",
                    "entity": destination.entity,
                    "resource_id": destination.resource_id,
                })));
            }
            let mut envelope = read.envelope;
            envelope.pending_rename_from = Some(source.resource_id.clone());
            envelope.resource_id = destination.resource_id.clone();
            envelope.relative_path = relative_path.to_string();
            envelope.generation = envelope.generation.saturating_add(1);
            envelope.publication_pending = true;
            let Some(moved) = self.swap(destination, None, &envelope)? else {
                continue;
            };
            if self.storage.compare_and_remove(source, &read.version)? {
                return Ok(Some(envelope.generation));
            }
            // The source changed after it was copied: withdraw the copy and
            // move what the source holds now.
            if !self.storage.compare_and_remove(destination, &moved)? {
                return Err(StoreError::conflict(format!(
                    "Collaboration documents {}/{} and {}/{} both changed while one moved to the other.",
                    source.entity, source.resource_id, destination.entity, destination.resource_id
                ))
                .with_data(serde_json::json!({
                    "code": "collaboration_move_contended",
                    "entity": source.entity,
                    "resource_id": source.resource_id,
                    "destination_resource_id": destination.resource_id,
                })));
            }
        }
    }

    pub fn update_relative_path(
        &self,
        document: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>> {
        loop {
            let Some(read) = self.read_envelope(document)? else {
                return Ok(None);
            };
            if read.envelope.relative_path == relative_path {
                return Ok(Some(read.envelope.generation));
            }
            let mut envelope = read.envelope;
            envelope.relative_path = relative_path.to_string();
            envelope.generation = envelope.generation.saturating_add(1);
            envelope.publication_pending = true;
            if self
                .swap(document, Some(&read.version), &envelope)?
                .is_some()
            {
                return Ok(Some(envelope.generation));
            }
        }
    }

    /// Redacted metadata for the stored documents, in entity and resource
    /// order. `limit` bounds how many are read; `None` reads every one. The
    /// flag reports whether more exist than were read.
    pub fn inspect(
        &self,
        entity: Option<&str>,
        resource_id: Option<&str>,
        limit: Option<usize>,
    ) -> StoreResult<(Vec<CollaborationDocumentInspection>, bool)> {
        let entities = entity
            .map(|value| vec![value.to_string()])
            .unwrap_or_else(|| {
                self.plans
                    .iter()
                    .map(|spec| spec.name.to_string())
                    .collect()
            });
        let sort = |inspections: &mut Vec<CollaborationDocumentInspection>| {
            inspections.sort_by(|left, right| {
                (&left.entity, &left.resource_id).cmp(&(&right.entity, &right.resource_id))
            });
        };
        if let Some(resource_id) = resource_id {
            let mut inspections = Vec::new();
            for entity_name in entities {
                let source = self
                    .storage
                    .source(&CollaborationDocumentId::new(&entity_name, resource_id));
                if let Some(stored) = self.storage.read(&source)? {
                    inspections.push(self.inspect_stored(
                        &entity_name,
                        Some(resource_id),
                        &source,
                        &stored.bytes,
                    ));
                }
            }
            sort(&mut inspections);
            return Ok((inspections, false));
        }

        let mut sources = Vec::new();
        for entity_name in entities {
            for source in self.storage.sources(&entity_name)? {
                sources.push((entity_name.clone(), source));
            }
        }
        sources.sort();
        let limit = limit.unwrap_or(sources.len());
        let truncated = sources.len() > limit;
        let mut inspections = Vec::new();
        for (entity_name, source) in sources.into_iter().take(limit) {
            if let Some(stored) = self.storage.read(&source)? {
                inspections.push(self.inspect_stored(&entity_name, None, &source, &stored.bytes));
            }
        }
        sort(&mut inspections);
        Ok((inspections, truncated))
    }

    fn inspect_stored(
        &self,
        entity: &str,
        requested_resource_id: Option<&str>,
        source: &str,
        bytes: &[u8],
    ) -> CollaborationDocumentInspection {
        let Ok(envelope) = serde_json::from_slice::<DurableCollaborationEnvelope>(bytes) else {
            return CollaborationDocumentInspection {
                entity: entity.to_string(),
                resource_id: requested_resource_id
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("storage:{}", &sha256_hex(source.as_bytes())[..16])),
                schema_version: 0,
                generation: 0,
                checkpoint_sequence: 0,
                compacted_through_sequence: 0,
                retained_operation_count: 0,
                checkpoint_bytes: bytes.len(),
                retained_operation_bytes: 0,
                publication_pending: false,
                migration_required: false,
                valid: false,
                failure_code: Some("invalid_envelope_json".to_string()),
            };
        };
        let document =
            CollaborationDocumentId::new(envelope.entity.clone(), envelope.resource_id.clone());
        let valid = validate_envelope(&document, &envelope).is_ok();
        inspection_from_envelope(
            self.plans,
            envelope,
            valid,
            (!valid).then(|| "collaboration_state_corrupt".to_string()),
        )
    }

    pub fn verify(&self, document: &CollaborationDocumentId) -> CollaborationDocumentInspection {
        let source = self.storage.source(document);
        match self.storage.read(&source) {
            Ok(Some(stored)) => self.inspect_stored(
                &document.entity,
                Some(&document.resource_id),
                &source,
                &stored.bytes,
            ),
            Ok(None) | Err(_) => CollaborationDocumentInspection {
                entity: document.entity.clone(),
                resource_id: document.resource_id.clone(),
                schema_version: 0,
                generation: 0,
                checkpoint_sequence: 0,
                compacted_through_sequence: 0,
                retained_operation_count: 0,
                checkpoint_bytes: 0,
                retained_operation_bytes: 0,
                publication_pending: false,
                migration_required: false,
                valid: false,
                failure_code: Some("collaboration_state_unreadable".to_string()),
            },
        }
    }

    pub fn counters(&self) -> CollaborationRuntimeCountersSnapshot {
        self.counters.snapshot()
    }

    /// `document`'s stored bytes, which need not be a readable envelope.
    fn stored(&self, document: &CollaborationDocumentId) -> StoreResult<StoredEnvelope> {
        self.storage
            .read(&self.storage.source(document))?
            .ok_or_else(|| missing_state(document))
    }

    pub fn export_for_recovery(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        self.audit_recovery_request(CollaborationRecoveryAction::Export, document, None, false)?;
        Ok(self.stored(document)?.bytes)
    }

    pub fn quarantine(
        &self,
        document: &CollaborationDocumentId,
        reason: &str,
    ) -> StoreResult<PathBuf> {
        self.audit_recovery_request(
            CollaborationRecoveryAction::Quarantine,
            document,
            Some(reason),
            false,
        )?;
        let stored = self.stored(document)?;
        self.storage
            .preserve(document, &evidence_label(reason), &stored.bytes)
    }

    /// Keeps the stored envelope as evidence, then drops its retained
    /// operations so the document reads from its checkpoint alone.
    pub fn repair(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf> {
        self.audit_recovery_request(
            CollaborationRecoveryAction::Repair,
            document,
            Some(reason),
            false,
        )?;
        let stored = self.stored(document)?;
        let mut envelope: DurableCollaborationEnvelope = serde_json::from_slice(&stored.bytes)
            .map_err(|_| {
                corrupt_state(
                    document,
                    "the envelope JSON cannot be repaired automatically",
                )
            })?;
        let _ = envelope.checkpoint_update(document)?;
        let evidence = self
            .storage
            .preserve(document, &evidence_label(reason), &stored.bytes)?;
        envelope.retained_operations.clear();
        envelope.compacted_through_sequence = envelope.checkpoint_sequence;
        envelope.generation = envelope.generation.saturating_add(1);
        envelope.publication_pending = true;
        if self
            .swap(document, Some(&stored.version), &envelope)?
            .is_none()
        {
            return Err(StoreError::conflict(format!(
                "Collaboration document {}/{} changed while it was being repaired.",
                document.entity, document.resource_id
            ))
            .with_data(serde_json::json!({
                "code": "collaboration_repair_contended",
                "entity": document.entity,
                "resource_id": document.resource_id,
            })));
        }
        Ok(evidence)
    }

    /// Re-reads every stored document and fingerprints the redacted catalogue.
    pub fn reindex(&self) -> StoreResult<(usize, String)> {
        self.audit_recovery_request(
            CollaborationRecoveryAction::Reindex,
            &CollaborationDocumentId::new("*", "*"),
            None,
            false,
        )?;
        let (documents, _) = self.inspect(None, None, None)?;
        let bytes = serde_json::to_vec(&documents)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        Ok((documents.len(), sha256_hex(&bytes)))
    }

    pub fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        self.storage.recovery_audit()
    }

    pub fn reset(&self, document: &CollaborationDocumentId) -> StoreResult<()> {
        self.audit_recovery_request(CollaborationRecoveryAction::Reset, document, None, true)?;
        self.delete(document)
    }

    fn audit_recovery_request(
        &self,
        action: CollaborationRecoveryAction,
        document: &CollaborationDocumentId,
        reason: Option<&str>,
        destructive: bool,
    ) -> StoreResult<()> {
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| StoreError::internal(error.to_string()))?
            .as_millis();
        self.storage
            .append_recovery_audit(&CollaborationRecoveryAuditRecord {
                timestamp_unix_ms,
                action,
                entity: document.entity.clone(),
                resource_id: document.resource_id.clone(),
                reason_sha256: reason.map(|value| sha256_hex(value.as_bytes())),
                destructive,
            })
    }

    fn record_loro_failure(&self, error: &CollaborationLoroError) {
        match error {
            CollaborationLoroError::MissingDependency => {
                self.counters
                    .dependency_blocks
                    .fetch_add(1, Ordering::Relaxed);
            }
            CollaborationLoroError::UnknownFrontier => {
                self.counters
                    .resync_requirements
                    .fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    pub fn bootstrap<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        relative_path: &str,
        seed: &Value,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<DurableCollaborationEnvelope, E> {
        if let Some(current) = self.load(document)? {
            return Ok(current);
        }
        let authoring = LoroAuthoringDocument::from_document(plan, seed)
            .map_err(|error| collaboration_loro_error(document, error))?;
        let accepted_update = BASE64
            .decode(
                authoring
                    .export_update_base64()
                    .map_err(|error| collaboration_loro_error(document, error))?,
            )
            .map_err(|error| StoreError::internal(error.to_string()))?;
        let etag = collaboration_etag(&accepted_update);
        let materialized = authoring
            .materialized_document(&etag)
            .map_err(|error| collaboration_loro_error(document, error))?;
        validate(&materialized)?;
        Ok(self.seed(plan, document, relative_path, accepted_update)?)
    }

    pub fn bootstrap_update<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        relative_path: &str,
        schema_version: u32,
        update_base64: &str,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<DurableCollaborationEnvelope, E> {
        if let Some(current) = self.load(document)? {
            return Ok(current);
        }
        let authoring = LoroAuthoringDocument::from_versioned_update_base64(
            plan,
            schema_version,
            update_base64,
        )
        .map_err(|error| collaboration_loro_error(document, error))?;
        let accepted_update = BASE64
            .decode(update_base64)
            .map_err(|error| StoreError::invalid_request(error.to_string()))?;
        let etag = collaboration_etag(&accepted_update);
        let materialized = authoring
            .materialized_document(&etag)
            .map_err(|error| collaboration_loro_error(document, error))?;
        validate(&materialized)?;
        Ok(self.seed(plan, document, relative_path, accepted_update)?)
    }

    pub fn migrate<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        relative_path: &str,
        from_schema_version: u32,
        migrated_update_base64: &str,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<DurableCollaborationEnvelope, E> {
        let migrated = LoroAuthoringDocument::from_versioned_update_base64(
            plan,
            plan.schema_version,
            migrated_update_base64,
        )
        .map_err(|error| collaboration_loro_error(document, error))?;
        let accepted_update = BASE64
            .decode(migrated_update_base64)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        let materialized = migrated
            .materialized_document(&collaboration_etag(&accepted_update))
            .map_err(|error| collaboration_loro_error(document, error))?;
        validate(&materialized)?;

        loop {
            let read = self
                .read_envelope(document)?
                .ok_or_else(|| missing_state(document))?;
            let current = &read.envelope;
            if current.schema_version == plan.schema_version {
                return Ok(read.envelope);
            }
            if current.schema_version != from_schema_version {
                return Err(StoreError::invalid_request(format!(
                    "Collaboration state for {}/{} uses schema version {}; migration expects {}.",
                    document.entity,
                    document.resource_id,
                    current.schema_version,
                    from_schema_version
                ))
                .with_data(serde_json::json!({
                    "code": "collaboration_migration_required",
                    "entity": document.entity,
                    "resource_id": document.resource_id,
                    "actual": current.schema_version,
                    "expected": plan.schema_version,
                }))
                .into());
            }
            let operation_id = format!(
                "migration:{}-to-{}:{}",
                from_schema_version,
                plan.schema_version,
                sha256_hex(&accepted_update)
            );
            match self.commit(
                Some(&read),
                CollaborationCommit {
                    document: document.clone(),
                    relative_path: relative_path.to_string(),
                    schema_version: plan.schema_version,
                    operation_id,
                    imported_update: accepted_update.clone(),
                    has_new_operations: true,
                    accepted_update: accepted_update.clone(),
                },
            )? {
                CollaborationCommitOutcome::Accepted(migrated)
                | CollaborationCommitOutcome::Duplicate(migrated) => return Ok(migrated),
                CollaborationCommitOutcome::Stale => continue,
            }
        }
    }

    /// Stores `accepted_update` as the first checkpoint of a document that
    /// has none, and returns the document's envelope either way.
    fn seed(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        relative_path: &str,
        accepted_update: Vec<u8>,
    ) -> StoreResult<DurableCollaborationEnvelope> {
        let operation_id = format!("seed:{}", sha256_hex(&accepted_update));
        loop {
            if let Some(current) = self.load(document)? {
                return Ok(current);
            }
            match self.commit(
                None,
                CollaborationCommit {
                    document: document.clone(),
                    relative_path: relative_path.to_string(),
                    schema_version: plan.schema_version,
                    operation_id: operation_id.clone(),
                    imported_update: accepted_update.clone(),
                    has_new_operations: true,
                    accepted_update: accepted_update.clone(),
                },
            )? {
                CollaborationCommitOutcome::Accepted(seeded)
                | CollaborationCommitOutcome::Duplicate(seeded) => return Ok(seeded),
                CollaborationCommitOutcome::Stale => continue,
            }
        }
    }
}

fn missing_state(document: &CollaborationDocumentId) -> StoreError {
    StoreError::not_found(format!(
        "No collaboration state exists for {}/{}.",
        document.entity, document.resource_id
    ))
}

/// Names recovery evidence by its reason without recording the reason.
fn evidence_label(reason: &str) -> String {
    sha256_hex(reason.as_bytes())[..12].to_string()
}

fn load_authoring_document(
    plan: &'static GeneratedCollaborationEntitySpec,
    document: &CollaborationDocumentId,
    envelope: &DurableCollaborationEnvelope,
) -> StoreResult<LoroAuthoringDocument> {
    if envelope.schema_version != plan.schema_version {
        return Err(StoreError::invalid_request(format!(
            "Collaboration state for {}/{} uses schema version {}; expected {}.",
            document.entity, document.resource_id, envelope.schema_version, plan.schema_version
        ))
        .with_data(serde_json::json!({
            "code": "collaboration_migration_required",
            "entity": document.entity,
            "resource_id": document.resource_id,
            "actual": envelope.schema_version,
            "expected": plan.schema_version,
        })));
    }
    let update_base64 = BASE64.encode(envelope.checkpoint_update(document)?);
    LoroAuthoringDocument::from_versioned_update_base64(
        plan,
        envelope.schema_version,
        &update_base64,
    )
    .map_err(|error| collaboration_loro_error(document, error))
}

fn collaboration_etag(accepted_update: &[u8]) -> String {
    format!("loro:{}", sha256_hex(accepted_update))
}

fn collaboration_loro_error(
    document: &CollaborationDocumentId,
    error: CollaborationLoroError,
) -> StoreError {
    match error {
        CollaborationLoroError::UnknownFrontier => StoreError::conflict(format!(
            "The collaboration frontier for {}/{} is no longer available; resynchronise without discarding the retained draft.",
            document.entity, document.resource_id
        ))
        .with_data(serde_json::json!({
            "code": "collaboration_resync_required",
            "entity": document.entity,
            "resource_id": document.resource_id,
            "draft_retained": true,
        })),
        CollaborationLoroError::MissingDependency => StoreError::conflict(format!(
            "The collaboration update for {}/{} has operations whose causal dependencies are missing; the draft was retained.",
            document.entity, document.resource_id
        ))
        .with_data(serde_json::json!({
            "code": "collaboration_dependency_blocked",
            "entity": document.entity,
            "resource_id": document.resource_id,
            "draft_retained": true,
        })),
        error => StoreError::invalid_request(error.to_string()).with_data(serde_json::json!({
            "code": "invalid_collaboration_update",
            "entity": document.entity,
            "resource_id": document.resource_id,
        })),
    }
}

/// Envelopes kept as files under one root, one per document, each replaced
/// atomically and durably under a per-document advisory lock that every
/// process sharing the root honours.
#[derive(Debug, Clone)]
pub struct LocalFileCollaborationStorage {
    root: PathBuf,
    #[cfg(test)]
    faults: CollaborationFaults,
}

impl LocalFileCollaborationStorage {
    /// Envelopes stored under `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            #[cfg(test)]
            faults: CollaborationFaults::default(),
        }
    }

    #[cfg(test)]
    fn inject_fault(&self, point: CollaborationFaultPoint) {
        self.faults.inject(point);
    }

    #[cfg(test)]
    fn fail_if(&self, point: CollaborationFaultPoint) -> StoreResult<()> {
        self.faults.fail_if(point)
    }

    fn entity_dir(&self, entity: &str) -> PathBuf {
        self.root.join(safe_entity_name(entity))
    }

    fn envelope_path(&self, document: &CollaborationDocumentId) -> PathBuf {
        self.entity_dir(&document.entity).join(format!(
            "{}.json",
            sha256_hex(document.resource_id.as_bytes())
        ))
    }

    fn lock_path(&self, document: &CollaborationDocumentId) -> PathBuf {
        self.entity_dir(&document.entity).join(format!(
            "{}.lock",
            sha256_hex(document.resource_id.as_bytes())
        ))
    }

    fn audit_path(&self) -> PathBuf {
        self.root.join("recovery-audit.ndjson")
    }

    fn audit_lock_path(&self) -> PathBuf {
        self.root.join("recovery-audit.lock")
    }

    fn with_document_lock<T>(
        &self,
        document: &CollaborationDocumentId,
        operation: impl FnOnce() -> StoreResult<T>,
    ) -> StoreResult<T> {
        let lock_path = self.lock_path(document);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                StoreError::internal(format!(
                    "Could not create collaboration storage directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                StoreError::internal(format!(
                    "Could not open collaboration lock {}: {error}",
                    lock_path.display()
                ))
            })?;
        lock.lock_exclusive().map_err(|error| {
            StoreError::internal(format!(
                "Could not lock collaboration document {}/{}: {error}",
                document.entity, document.resource_id
            ))
        })?;
        let result = operation();
        let unlock_result = FileExt::unlock(&lock).map_err(|error| {
            StoreError::internal(format!(
                "Could not unlock collaboration document {}/{}: {error}",
                document.entity, document.resource_id
            ))
        });
        match (result, unlock_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn write_envelope(&self, path: &Path, bytes: &[u8]) -> StoreResult<()> {
        #[cfg(test)]
        {
            write_atomic_durable_with_hooks(
                path,
                bytes,
                || self.fail_if(CollaborationFaultPoint::BeforeTemporaryWrite),
                || self.fail_if(CollaborationFaultPoint::AfterTemporarySync),
                || self.fail_if(CollaborationFaultPoint::AfterRenameBeforeDirectorySync),
            )
        }
        #[cfg(not(test))]
        write_atomic_durable(path, bytes)
    }
}

/// The bytes at `path`, or `None` when there is no file.
fn read_optional(path: &Path) -> StoreResult<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StoreError::internal(format!(
            "Could not read collaboration state {}: {error}",
            path.display()
        ))),
    }
}

impl CollaborationStoragePort for LocalFileCollaborationStorage {
    fn source(&self, document: &CollaborationDocumentId) -> String {
        self.envelope_path(document).display().to_string()
    }

    fn sources(&self, entity: &str) -> StoreResult<Vec<String>> {
        let directory = self.entity_dir(entity);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(StoreError::internal(format!(
                    "Could not list collaboration state {}: {error}",
                    directory.display()
                )))
            }
        };
        let mut sources = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| {
                    StoreError::internal(format!(
                        "Could not list collaboration state {}: {error}",
                        directory.display()
                    ))
                })?
                .path();
            if path.extension().and_then(|value| value.to_str()) == Some("json") {
                sources.push(path.display().to_string());
            }
        }
        sources.sort();
        Ok(sources)
    }

    fn read(&self, source: &str) -> StoreResult<Option<StoredEnvelope>> {
        Ok(
            read_optional(Path::new(source))?.map(|bytes| StoredEnvelope {
                version: sha256_hex(&bytes),
                bytes,
            }),
        )
    }

    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>> {
        let path = self.envelope_path(document);
        self.with_document_lock(document, || {
            let current = read_optional(&path)?;
            if current.as_deref().map(sha256_hex).as_deref() != expected {
                return Ok(None);
            }
            self.write_envelope(&path, bytes)?;
            Ok(Some(sha256_hex(bytes)))
        })
    }

    fn compare_and_remove(
        &self,
        document: &CollaborationDocumentId,
        expected: &str,
    ) -> StoreResult<bool> {
        let path = self.envelope_path(document);
        self.with_document_lock(document, || {
            let current = read_optional(&path)?;
            if current.as_deref().map(sha256_hex).as_deref() != Some(expected) {
                return Ok(false);
            }
            remove_file_if_exists(&path)?;
            Ok(true)
        })
    }

    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<PathBuf> {
        let destination = self.root.join("quarantine").join(format!(
            "{}-{}-{}-{}.json",
            safe_entity_name(&document.entity),
            sha256_hex(document.resource_id.as_bytes()),
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
            label
        ));
        write_atomic_durable(&destination, bytes)?;
        Ok(destination)
    }

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()> {
        std::fs::create_dir_all(&self.root).map_err(|error| {
            StoreError::internal(format!(
                "Could not create collaboration audit directory {}: {error}",
                self.root.display()
            ))
        })?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.audit_lock_path())
            .map_err(|error| {
                StoreError::internal(format!(
                    "Could not open collaboration recovery audit lock: {error}"
                ))
            })?;
        lock.lock_exclusive().map_err(|error| {
            StoreError::internal(format!(
                "Could not lock collaboration recovery audit: {error}"
            ))
        })?;
        let result = (|| {
            let mut line = serde_json::to_vec(record)
                .map_err(|error| StoreError::internal(error.to_string()))?;
            line.push(b'\n');
            let mut audit = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.audit_path())
                .map_err(|error| {
                    StoreError::internal(format!(
                        "Could not open collaboration recovery audit: {error}"
                    ))
                })?;
            audit.write_all(&line).map_err(|error| {
                StoreError::internal(format!(
                    "Could not append collaboration recovery audit: {error}"
                ))
            })?;
            audit.sync_all().map_err(|error| {
                StoreError::internal(format!(
                    "Could not sync collaboration recovery audit: {error}"
                ))
            })
        })();
        let _ = FileExt::unlock(&lock);
        result
    }

    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        match std::fs::read_to_string(self.audit_path()) {
            Ok(contents) => contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| {
                    serde_json::from_str(line)
                        .map_err(|error| StoreError::internal(error.to_string()))
                })
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(StoreError::internal(format!(
                "Could not read collaboration recovery audit: {error}"
            ))),
        }
    }
}

fn inspection_from_envelope(
    plans: &[GeneratedCollaborationEntitySpec],
    envelope: DurableCollaborationEnvelope,
    valid: bool,
    failure_code: Option<String>,
) -> CollaborationDocumentInspection {
    let expected_schema_version = plans
        .iter()
        .find(|plan| plan.name == envelope.entity)
        .map(|plan| plan.schema_version);
    CollaborationDocumentInspection {
        entity: envelope.entity,
        resource_id: envelope.resource_id,
        schema_version: envelope.schema_version,
        generation: envelope.generation,
        checkpoint_sequence: envelope.checkpoint_sequence,
        compacted_through_sequence: envelope.compacted_through_sequence,
        retained_operation_count: envelope.retained_operations.len(),
        checkpoint_bytes: envelope.checkpoint_bytes,
        retained_operation_bytes: envelope
            .retained_operations
            .iter()
            .map(|operation| operation.update_bytes)
            .sum(),
        publication_pending: envelope.publication_pending,
        migration_required: expected_schema_version
            .is_some_and(|expected| expected != envelope.schema_version),
        valid,
        failure_code,
    }
}

fn validate_envelope(
    document: &CollaborationDocumentId,
    envelope: &DurableCollaborationEnvelope,
) -> StoreResult<()> {
    if envelope.envelope_version != ENVELOPE_VERSION {
        return Err(corrupt_state(
            document,
            format!(
                "unsupported durable envelope version {}",
                envelope.envelope_version
            ),
        ));
    }
    if envelope.entity != document.entity || envelope.resource_id != document.resource_id {
        return Err(corrupt_state(
            document,
            format!(
                "envelope identifies {}/{}, expected {}/{}",
                envelope.entity, envelope.resource_id, document.entity, document.resource_id
            ),
        ));
    }
    if envelope.schema_version == 0 {
        return Err(corrupt_state(
            document,
            "collaboration schema version must be explicit",
        ));
    }
    let _ = envelope.checkpoint_update(document)?;
    let mut previous_sequence = envelope.compacted_through_sequence;
    for operation in &envelope.retained_operations {
        if operation.sequence <= previous_sequence {
            return Err(corrupt_state(
                document,
                "retained operation sequences are not strictly increasing",
            ));
        }
        if operation.schema_version != envelope.schema_version {
            return Err(corrupt_state(
                document,
                format!(
                    "operation {} uses schema version {}, envelope uses {}",
                    operation.operation_id, operation.schema_version, envelope.schema_version
                ),
            ));
        }
        let _ = operation.decode(document)?;
        previous_sequence = operation.sequence;
    }
    if previous_sequence != envelope.checkpoint_sequence {
        return Err(corrupt_state(
            document,
            "retained operation window does not end at the checkpoint sequence",
        ));
    }
    Ok(())
}

fn corrupt_state(document: &CollaborationDocumentId, message: impl Into<String>) -> StoreError {
    StoreError::internal(format!(
        "Collaboration state for {}/{} is corrupt: {}. Use collaboration recovery tooling to inspect or quarantine it; authoritative state was not replaced with an empty document.",
        document.entity,
        document.resource_id,
        message.into()
    ))
    .with_data(serde_json::json!({
        "code": "collaboration_state_corrupt",
        "entity": document.entity,
        "resource_id": document.resource_id,
        "recovery_required": true,
    }))
}

pub fn write_atomic_durable(path: &Path, bytes: &[u8]) -> StoreResult<()> {
    write_atomic_durable_with_hooks(path, bytes, || Ok(()), || Ok(()), || Ok(()))
}

fn write_atomic_durable_with_hooks(
    path: &Path,
    bytes: &[u8],
    before_temporary_write: impl FnOnce() -> StoreResult<()>,
    after_temporary_sync: impl FnOnce() -> StoreResult<()>,
    after_rename: impl FnOnce() -> StoreResult<()>,
) -> StoreResult<()> {
    let Some(parent) = path.parent() else {
        return Err(StoreError::internal(format!(
            "Collaboration state path {} has no parent.",
            path.display()
        )));
    };
    std::fs::create_dir_all(parent).map_err(|error| {
        StoreError::internal(format!(
            "Could not create collaboration storage {}: {error}",
            parent.display()
        ))
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("collaboration"),
        Uuid::new_v4()
    ));
    before_temporary_write()?;
    let mut file = File::create(&temporary).map_err(|error| {
        StoreError::internal(format!(
            "Could not create collaboration transaction {}: {error}",
            temporary.display()
        ))
    })?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(StoreError::internal(format!(
            "Could not durably write collaboration transaction {}: {error}",
            temporary.display()
        )));
    }
    if let Err(error) = after_temporary_sync() {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(StoreError::internal(format!(
            "Could not atomically publish collaboration state {}: {error}",
            path.display()
        )));
    }
    after_rename()?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            StoreError::internal(format!(
                "Could not durably publish collaboration directory {}: {error}",
                parent.display()
            ))
        })
}

fn remove_file_if_exists(path: &Path) -> StoreResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::internal(format!(
            "Could not remove collaboration state {}: {error}",
            path.display()
        ))),
    }
}

fn safe_entity_name(entity: &str) -> String {
    entity
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests;

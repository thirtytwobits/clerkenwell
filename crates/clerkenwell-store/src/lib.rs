//! Durable single-authority storage for collaborative documents.
//!
//! One envelope per document holds a checksummed checkpoint and a window of
//! retained operations. A commit imports an update into the accepted replica,
//! judges it against the plan's field policies, validates the materialised
//! document, and swaps the envelope under a per-document lock with an atomic
//! durable write. Publication is acknowledged separately from the commit.
//! Corruption is reported, never read as an empty document, and recovery
//! preserves evidence and records an audit trail.

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

#[derive(Debug, Clone)]
pub struct CollaborationCommit {
    pub document: CollaborationDocumentId,
    pub relative_path: String,
    pub schema_version: u32,
    pub expected_generation: u64,
    pub operation_id: String,
    pub imported_update: Vec<u8>,
    pub has_new_operations: bool,
    pub accepted_update: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum CollaborationCommitOutcome {
    Accepted,
    Duplicate,
    Stale,
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

pub trait CollaborationStoragePort: std::fmt::Debug + Send + Sync {
    fn load(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<DurableCollaborationEnvelope>>;

    fn compare_and_swap(
        &self,
        commit: CollaborationCommit,
    ) -> StoreResult<CollaborationCommitOutcome>;

    fn acknowledge_publication(
        &self,
        document: &CollaborationDocumentId,
        generation: u64,
    ) -> StoreResult<()>;

    fn delete(&self, document: &CollaborationDocumentId) -> StoreResult<()>;

    fn move_document(
        &self,
        source: &CollaborationDocumentId,
        destination: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>>;

    fn update_relative_path(
        &self,
        document: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>>;

    fn list_publications(&self, entity: &str) -> StoreResult<CollaborationPublicationScan>;

    fn list_entity(&self, entity: &str) -> StoreResult<Vec<DurableCollaborationEnvelope>>;

    fn export_for_recovery(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>>;

    fn quarantine(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf>;

    /// Redacted metadata for the stored documents, in entity and resource
    /// order. `limit` bounds how many are read; `None` reads every one. The
    /// flag reports whether more exist than were read.
    fn inspect(
        &self,
        entity: Option<&str>,
        resource_id: Option<&str>,
        limit: Option<usize>,
    ) -> StoreResult<(Vec<CollaborationDocumentInspection>, bool)>;

    fn verify(&self, document: &CollaborationDocumentId) -> CollaborationDocumentInspection;

    fn repair(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf>;

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
        let storage = LocalFileCollaborationStorage::new(root, self.plans);
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
        let storage = LocalFileCollaborationStorage::new(root, plans);
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

    pub fn detail(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<Value>> {
        let Some(envelope) = self.storage.load(document)? else {
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
        let envelope = self.storage.load(document)?.ok_or_else(|| {
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
            let current = match self.storage.load(&request.document)? {
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
                Some(current) => current,
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
            let outcome = self.storage.compare_and_swap(CollaborationCommit {
                document: request.document.clone(),
                relative_path: request.relative_path.clone(),
                schema_version: request.schema_version,
                expected_generation: current.generation,
                operation_id: request.operation_id.clone(),
                imported_update: imported_update.clone(),
                has_new_operations: before_import != authoring.accepted_frontier_base64(),
                accepted_update: accepted_update.clone(),
            })?;
            if matches!(outcome, CollaborationCommitOutcome::Stale) {
                continue;
            }
            #[cfg(test)]
            self.faults
                .fail_if(CollaborationFaultPoint::AfterDurableCommit)?;
            let duplicate = matches!(outcome, CollaborationCommitOutcome::Duplicate);
            if duplicate {
                self.counters
                    .duplicate_imports
                    .fetch_add(1, Ordering::Relaxed);
            }
            let accepted = self
                .storage
                .load(&request.document)?
                .ok_or_else(|| StoreError::internal("Accepted collaboration state disappeared."))?;
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
        self.storage.acknowledge_publication(document, generation)
    }

    pub fn publication_scan(&self) -> StoreResult<CollaborationPublicationScan> {
        let mut scan = CollaborationPublicationScan::default();
        for spec in self.plans {
            let entity_scan = self.storage.list_publications(spec.name)?;
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

    pub fn delete(&self, document: &CollaborationDocumentId) -> StoreResult<()> {
        self.storage.delete(document)
    }

    pub fn move_document(
        &self,
        source: &CollaborationDocumentId,
        destination: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>> {
        self.storage
            .move_document(source, destination, relative_path)
    }

    pub fn update_relative_path(
        &self,
        document: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>> {
        self.storage.update_relative_path(document, relative_path)
    }

    pub fn inspect(
        &self,
        entity: Option<&str>,
        resource_id: Option<&str>,
        limit: Option<usize>,
    ) -> StoreResult<(Vec<CollaborationDocumentInspection>, bool)> {
        self.storage.inspect(entity, resource_id, limit)
    }

    pub fn verify(&self, document: &CollaborationDocumentId) -> CollaborationDocumentInspection {
        self.storage.verify(document)
    }

    pub fn counters(&self) -> CollaborationRuntimeCountersSnapshot {
        self.counters.snapshot()
    }

    pub fn export_for_recovery(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        self.audit_recovery_request(CollaborationRecoveryAction::Export, document, None, false)?;
        self.storage.export_for_recovery(document)
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
        self.storage.quarantine(document, reason)
    }

    pub fn repair(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf> {
        self.audit_recovery_request(
            CollaborationRecoveryAction::Repair,
            document,
            Some(reason),
            false,
        )?;
        self.storage.repair(document, reason)
    }

    /// Re-reads every stored document and fingerprints the redacted catalogue.
    pub fn reindex(&self) -> StoreResult<(usize, String)> {
        self.audit_recovery_request(
            CollaborationRecoveryAction::Reindex,
            &CollaborationDocumentId::new("*", "*"),
            None,
            false,
        )?;
        let (documents, _) = self.storage.inspect(None, None, None)?;
        let bytes = serde_json::to_vec(&documents)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        Ok((documents.len(), sha256_hex(&bytes)))
    }

    pub fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        self.storage.recovery_audit()
    }

    pub fn reset(&self, document: &CollaborationDocumentId) -> StoreResult<()> {
        self.audit_recovery_request(CollaborationRecoveryAction::Reset, document, None, true)?;
        self.storage.delete(document)
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
        if let Some(current) = self.storage.load(document)? {
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
        let operation_id = format!("seed:{}", sha256_hex(&accepted_update));
        match self.storage.compare_and_swap(CollaborationCommit {
            document: document.clone(),
            relative_path: relative_path.to_string(),
            schema_version: plan.schema_version,
            expected_generation: 0,
            operation_id,
            imported_update: accepted_update.clone(),
            has_new_operations: true,
            accepted_update,
        })? {
            CollaborationCommitOutcome::Accepted | CollaborationCommitOutcome::Duplicate => {
                self.storage.load(document)?.ok_or_else(|| {
                    StoreError::internal("Seeded collaboration state disappeared.").into()
                })
            }
            CollaborationCommitOutcome::Stale => self.storage.load(document)?.ok_or_else(|| {
                StoreError::internal("Concurrent collaboration seed disappeared.").into()
            }),
        }
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
        if let Some(current) = self.storage.load(document)? {
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
        let operation_id = format!("seed:{}", sha256_hex(&accepted_update));
        match self.storage.compare_and_swap(CollaborationCommit {
            document: document.clone(),
            relative_path: relative_path.to_string(),
            schema_version: plan.schema_version,
            expected_generation: 0,
            operation_id,
            imported_update: accepted_update.clone(),
            has_new_operations: true,
            accepted_update,
        })? {
            CollaborationCommitOutcome::Accepted | CollaborationCommitOutcome::Duplicate => {
                self.storage.load(document)?.ok_or_else(|| {
                    StoreError::internal("Seeded collaboration state disappeared.").into()
                })
            }
            CollaborationCommitOutcome::Stale => self.storage.load(document)?.ok_or_else(|| {
                StoreError::internal("Concurrent collaboration seed disappeared.").into()
            }),
        }
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
            let current = self.storage.load(document)?.ok_or_else(|| {
                StoreError::not_found(format!(
                    "No collaboration state exists for {}/{}.",
                    document.entity, document.resource_id
                ))
            })?;
            if current.schema_version == plan.schema_version {
                return Ok(current);
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
            match self.storage.compare_and_swap(CollaborationCommit {
                document: document.clone(),
                relative_path: relative_path.to_string(),
                schema_version: plan.schema_version,
                expected_generation: current.generation,
                operation_id,
                imported_update: accepted_update.clone(),
                has_new_operations: true,
                accepted_update: accepted_update.clone(),
            })? {
                CollaborationCommitOutcome::Accepted | CollaborationCommitOutcome::Duplicate => {
                    return self.storage.load(document)?.ok_or_else(|| {
                        StoreError::internal("Migrated collaboration state disappeared.").into()
                    });
                }
                CollaborationCommitOutcome::Stale => continue,
            }
        }
    }
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

#[derive(Debug, Clone)]
pub struct LocalFileCollaborationStorage {
    root: PathBuf,
    plans: &'static [GeneratedCollaborationEntitySpec],
    #[cfg(test)]
    faults: CollaborationFaults,
}

impl LocalFileCollaborationStorage {
    /// Envelopes for documents of `plans`, stored under `root`.
    pub fn new(root: &Path, plans: &'static [GeneratedCollaborationEntitySpec]) -> Self {
        Self {
            root: root.to_path_buf(),
            plans,
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

    fn with_document_pair_lock<T>(
        &self,
        first: &CollaborationDocumentId,
        second: &CollaborationDocumentId,
        operation: impl FnOnce() -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut documents = [first, second];
        documents.sort_by(|left, right| {
            (&left.entity, &left.resource_id).cmp(&(&right.entity, &right.resource_id))
        });
        let mut locks = Vec::with_capacity(documents.len());
        for document in documents {
            let lock_path = self.lock_path(document);
            if let Some(parent) = lock_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| StoreError::internal(error.to_string()))?;
            }
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|error| StoreError::internal(error.to_string()))?;
            lock.lock_exclusive()
                .map_err(|error| StoreError::internal(error.to_string()))?;
            locks.push(lock);
        }
        let result = operation();
        for lock in locks.iter().rev() {
            FileExt::unlock(lock).map_err(|error| StoreError::internal(error.to_string()))?;
        }
        result
    }

    fn read_path(
        &self,
        document: &CollaborationDocumentId,
        path: &Path,
    ) -> StoreResult<Option<DurableCollaborationEnvelope>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StoreError::internal(format!(
                    "Could not read collaboration state {}: {error}",
                    path.display()
                )))
            }
        };
        let envelope: DurableCollaborationEnvelope =
            serde_json::from_slice(&bytes).map_err(|error| {
                corrupt_state(
                    document,
                    format!("envelope {} is invalid JSON: {error}", path.display()),
                )
            })?;
        validate_envelope(document, &envelope)?;
        Ok(Some(envelope))
    }

    fn write_envelope(
        &self,
        document: &CollaborationDocumentId,
        envelope: &DurableCollaborationEnvelope,
    ) -> StoreResult<()> {
        validate_envelope(document, envelope)?;
        let bytes = serde_json::to_vec_pretty(envelope)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        #[cfg(test)]
        {
            write_atomic_durable_with_hooks(
                &self.envelope_path(document),
                &bytes,
                || self.fail_if(CollaborationFaultPoint::BeforeTemporaryWrite),
                || self.fail_if(CollaborationFaultPoint::AfterTemporarySync),
                || self.fail_if(CollaborationFaultPoint::AfterRenameBeforeDirectorySync),
            )
        }
        #[cfg(not(test))]
        write_atomic_durable(&self.envelope_path(document), &bytes)
    }

    pub fn install_migrated(
        &self,
        envelope: DurableCollaborationEnvelope,
    ) -> StoreResult<DurableCollaborationEnvelope> {
        let document =
            CollaborationDocumentId::new(envelope.entity.clone(), envelope.resource_id.clone());
        self.with_document_lock(&document, || {
            if let Some(current) = self.read_path(&document, &self.envelope_path(&document))? {
                return Ok(current);
            }
            self.write_envelope(&document, &envelope)?;
            Ok(envelope)
        })
    }

    fn inspect(
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
        if let Some(resource_id) = resource_id {
            let mut inspections = Vec::new();
            for entity_name in entities {
                let document = CollaborationDocumentId::new(&entity_name, resource_id);
                let path = self.envelope_path(&document);
                if path.exists() {
                    inspections.push(self.inspect_path(&entity_name, Some(resource_id), &path)?);
                }
            }
            inspections.sort_by(|left, right| {
                (&left.entity, &left.resource_id).cmp(&(&right.entity, &right.resource_id))
            });
            return Ok((inspections, false));
        }

        let mut inspections = Vec::new();
        let mut paths = Vec::new();
        for entity_name in entities {
            let directory = self.entity_dir(&entity_name);
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(StoreError::internal(format!(
                        "Could not inspect collaboration state {}: {error}",
                        directory.display()
                    )))
                }
            };
            for entry in entries {
                let entry = entry.map_err(|error| StoreError::internal(error.to_string()))?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                paths.push((entity_name.clone(), path));
            }
        }
        paths.sort();
        let limit = limit.unwrap_or(paths.len());
        let truncated = paths.len() > limit;
        for (entity_name, path) in paths.into_iter().take(limit) {
            inspections.push(self.inspect_path(&entity_name, None, &path)?);
        }
        inspections.sort_by(|left, right| {
            (&left.entity, &left.resource_id).cmp(&(&right.entity, &right.resource_id))
        });
        Ok((inspections, truncated))
    }

    fn inspect_path(
        &self,
        entity: &str,
        requested_resource_id: Option<&str>,
        path: &Path,
    ) -> StoreResult<CollaborationDocumentInspection> {
        let bytes = std::fs::read(path).map_err(|error| {
            StoreError::internal(format!(
                "Could not inspect collaboration state {}: {error}",
                path.display()
            ))
        })?;
        let envelope = match serde_json::from_slice::<DurableCollaborationEnvelope>(&bytes) {
            Ok(envelope) => envelope,
            Err(_) => {
                return Ok(CollaborationDocumentInspection {
                    entity: entity.to_string(),
                    resource_id: requested_resource_id
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            let fingerprint = sha256_hex(
                                path.file_name()
                                    .and_then(|value| value.to_str())
                                    .unwrap_or("unknown")
                                    .as_bytes(),
                            );
                            format!("storage:{}", &fingerprint[..16])
                        }),
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
                });
            }
        };
        let document =
            CollaborationDocumentId::new(envelope.entity.clone(), envelope.resource_id.clone());
        let valid = validate_envelope(&document, &envelope).is_ok();
        Ok(inspection_from_envelope(
            self.plans,
            envelope,
            valid,
            (!valid).then(|| "collaboration_state_corrupt".to_string()),
        ))
    }

    fn verify(&self, document: &CollaborationDocumentId) -> CollaborationDocumentInspection {
        match self.inspect_path(
            &document.entity,
            Some(&document.resource_id),
            &self.envelope_path(document),
        ) {
            Ok(inspection) => inspection,
            Err(_) => CollaborationDocumentInspection {
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

    fn repair(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf> {
        self.with_document_lock(document, || {
            let source = self.envelope_path(document);
            let bytes = std::fs::read(&source).map_err(|error| {
                StoreError::internal(format!(
                    "Could not read collaboration state for repair {}/{}: {error}",
                    document.entity, document.resource_id
                ))
            })?;
            let mut envelope: DurableCollaborationEnvelope = serde_json::from_slice(&bytes)
                .map_err(|_| {
                    corrupt_state(
                        document,
                        "the envelope JSON cannot be repaired automatically",
                    )
                })?;
            let _ = envelope.checkpoint_update(document)?;
            let evidence = self.copy_to_quarantine_unlocked(document, reason)?;
            envelope.retained_operations.clear();
            envelope.compacted_through_sequence = envelope.checkpoint_sequence;
            envelope.generation = envelope.generation.saturating_add(1);
            envelope.publication_pending = true;
            self.write_envelope(document, &envelope)?;
            Ok(evidence)
        })
    }

    fn copy_to_quarantine_unlocked(
        &self,
        document: &CollaborationDocumentId,
        reason: &str,
    ) -> StoreResult<PathBuf> {
        let source = self.envelope_path(document);
        if !source.exists() {
            return Err(StoreError::not_found(format!(
                "No collaboration state exists for {}/{}.",
                document.entity, document.resource_id
            )));
        }
        let quarantine_dir = self.root.join("quarantine");
        std::fs::create_dir_all(&quarantine_dir).map_err(|error| {
            StoreError::internal(format!(
                "Could not create collaboration quarantine {}: {error}",
                quarantine_dir.display()
            ))
        })?;
        let reason_hash = &sha256_hex(reason.as_bytes())[..12];
        let destination = quarantine_dir.join(format!(
            "{}-{}-{}-{}.json",
            safe_entity_name(&document.entity),
            sha256_hex(document.resource_id.as_bytes()),
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
            reason_hash
        ));
        std::fs::copy(&source, &destination).map_err(|error| {
            StoreError::internal(format!(
                "Could not preserve collaboration evidence {}: {error}",
                destination.display()
            ))
        })?;
        Ok(destination)
    }
}

impl CollaborationStoragePort for LocalFileCollaborationStorage {
    fn load(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<DurableCollaborationEnvelope>> {
        self.read_path(document, &self.envelope_path(document))
    }

    fn compare_and_swap(
        &self,
        commit: CollaborationCommit,
    ) -> StoreResult<CollaborationCommitOutcome> {
        let document = commit.document.clone();
        self.with_document_lock(&document, || {
            let current = self.read_path(&document, &self.envelope_path(&document))?;
            let current_generation = current.as_ref().map_or(0, |state| state.generation);
            if current_generation != commit.expected_generation {
                return current
                    .map(|_| CollaborationCommitOutcome::Stale)
                    .ok_or_else(|| {
                        StoreError::conflict(format!(
                            "Collaboration document {}/{} changed before commit.",
                            document.entity, document.resource_id
                        ))
                    });
            }
            let operation_id = commit.operation_id;
            if let Some(current) = current.as_ref() {
                if let Some(existing) = current
                    .retained_operations
                    .iter()
                    .find(|operation| operation.operation_id == operation_id)
                {
                    if existing.update_sha256 == sha256_hex(&commit.imported_update) {
                        return Ok(CollaborationCommitOutcome::Duplicate);
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
                    || current.checkpoint_sha256 == sha256_hex(&commit.accepted_update)
                {
                    return Ok(CollaborationCommitOutcome::Duplicate);
                }
            }
            let sequence = current
                .as_ref()
                .map_or(1, |state| state.checkpoint_sequence.saturating_add(1));
            let schema_changed = current
                .as_ref()
                .is_some_and(|state| state.schema_version != commit.schema_version);
            let mut retained_operations = if schema_changed {
                Vec::new()
            } else {
                current
                    .as_ref()
                    .map_or_else(Vec::new, |state| state.retained_operations.clone())
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
                generation: current_generation.saturating_add(1),
                checkpoint_sequence: sequence,
                compacted_through_sequence: retained_from.saturating_sub(1),
                checkpoint_update_base64: BASE64.encode(&commit.accepted_update),
                checkpoint_sha256: sha256_hex(&commit.accepted_update),
                checkpoint_bytes: commit.accepted_update.len(),
                retained_operations,
                publication_pending: true,
                pending_rename_from: None,
            };
            self.write_envelope(&document, &envelope)?;
            Ok(CollaborationCommitOutcome::Accepted)
        })
    }

    fn acknowledge_publication(
        &self,
        document: &CollaborationDocumentId,
        generation: u64,
    ) -> StoreResult<()> {
        #[cfg(test)]
        self.fail_if(CollaborationFaultPoint::BeforePublicationAcknowledgement)?;
        self.with_document_lock(document, || {
            let Some(mut envelope) = self.read_path(document, &self.envelope_path(document))?
            else {
                return Err(StoreError::not_found(format!(
                    "No collaboration state exists for {}/{}.",
                    document.entity, document.resource_id
                )));
            };
            if envelope.generation != generation {
                return Ok(());
            }
            if envelope.publication_pending {
                envelope.publication_pending = false;
                self.write_envelope(document, &envelope)?;
            }
            Ok(())
        })
    }

    fn delete(&self, document: &CollaborationDocumentId) -> StoreResult<()> {
        self.with_document_lock(document, || {
            remove_file_if_exists(&self.envelope_path(document))
        })
    }

    fn move_document(
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
        self.with_document_pair_lock(source, destination, || {
            let Some(mut envelope) = self.read_path(source, &self.envelope_path(source))? else {
                return Ok(None);
            };
            if self
                .read_path(destination, &self.envelope_path(destination))?
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
            envelope.pending_rename_from = Some(source.resource_id.clone());
            envelope.resource_id = destination.resource_id.clone();
            envelope.relative_path = relative_path.to_string();
            envelope.generation = envelope.generation.saturating_add(1);
            envelope.publication_pending = true;
            let generation = envelope.generation;
            self.write_envelope(destination, &envelope)?;
            remove_file_if_exists(&self.envelope_path(source))?;
            Ok(Some(generation))
        })
    }

    fn update_relative_path(
        &self,
        document: &CollaborationDocumentId,
        relative_path: &str,
    ) -> StoreResult<Option<u64>> {
        self.with_document_lock(document, || {
            let Some(mut envelope) = self.read_path(document, &self.envelope_path(document))?
            else {
                return Ok(None);
            };
            if envelope.relative_path == relative_path {
                return Ok(Some(envelope.generation));
            }
            envelope.relative_path = relative_path.to_string();
            envelope.generation = envelope.generation.saturating_add(1);
            envelope.publication_pending = true;
            let generation = envelope.generation;
            self.write_envelope(document, &envelope)?;
            Ok(Some(generation))
        })
    }

    fn list_publications(&self, entity: &str) -> StoreResult<CollaborationPublicationScan> {
        let directory = self.entity_dir(entity);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CollaborationPublicationScan::default())
            }
            Err(error) => {
                return Err(StoreError::internal(format!(
                    "Could not list collaboration publications {}: {error}",
                    directory.display()
                )))
            }
        };
        let mut scan = CollaborationPublicationScan::default();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    let message = format!(
                        "Could not read a collaboration publication entry in {}: {error}",
                        directory.display()
                    );
                    scan.problems.push(CollaborationPublicationScanProblem {
                        source: directory.display().to_string(),
                        fingerprint: sha256_hex(message.as_bytes()),
                        code: "collaboration_publication_entry_unreadable",
                        message,
                    });
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    let message = format!(
                        "Could not read collaboration publication {}: {error}",
                        path.display()
                    );
                    scan.problems.push(CollaborationPublicationScanProblem {
                        source: path.display().to_string(),
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
                    let message = format!(
                        "Collaboration publication {} is invalid JSON: {error}",
                        path.display()
                    );
                    scan.problems.push(CollaborationPublicationScanProblem {
                        source: path.display().to_string(),
                        fingerprint: sha256_hex(&bytes),
                        code: "collaboration_publication_invalid_json",
                        message,
                    });
                    continue;
                }
            };
            let document =
                CollaborationDocumentId::new(header.entity.clone(), header.resource_id.clone());
            if header.envelope_version != ENVELOPE_VERSION
                || header.entity != entity
                || path.file_name().and_then(|value| value.to_str())
                    != Some(format!("{}.json", sha256_hex(header.resource_id.as_bytes())).as_str())
            {
                let message = format!(
                    "Collaboration publication header in {} does not match its storage identity",
                    path.display()
                );
                scan.problems.push(CollaborationPublicationScanProblem {
                    source: path.display().to_string(),
                    fingerprint: sha256_hex(&bytes),
                    code: "collaboration_state_corrupt",
                    message,
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
        scan.publications
            .sort_by(|left, right| left.document.resource_id.cmp(&right.document.resource_id));
        scan.problems
            .sort_by(|left, right| left.source.cmp(&right.source));
        Ok(scan)
    }

    fn list_entity(&self, entity: &str) -> StoreResult<Vec<DurableCollaborationEnvelope>> {
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
        let mut envelopes = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| StoreError::internal(error.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|error| {
                StoreError::internal(format!(
                    "Could not read collaboration state {}: {error}",
                    path.display()
                ))
            })?;
            let envelope: DurableCollaborationEnvelope =
                serde_json::from_slice(&bytes).map_err(|error| {
                    StoreError::internal(format!(
                        "Collaboration envelope {} is invalid JSON: {error}",
                        path.display()
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

    fn export_for_recovery(&self, document: &CollaborationDocumentId) -> StoreResult<Vec<u8>> {
        std::fs::read(self.envelope_path(document))
            .map_err(|error| StoreError::internal(error.to_string()))
    }

    fn quarantine(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf> {
        self.with_document_lock(document, || {
            self.copy_to_quarantine_unlocked(document, reason)
        })
    }

    fn inspect(
        &self,
        entity: Option<&str>,
        resource_id: Option<&str>,
        limit: Option<usize>,
    ) -> StoreResult<(Vec<CollaborationDocumentInspection>, bool)> {
        LocalFileCollaborationStorage::inspect(self, entity, resource_id, limit)
    }

    fn verify(&self, document: &CollaborationDocumentId) -> CollaborationDocumentInspection {
        LocalFileCollaborationStorage::verify(self, document)
    }

    fn repair(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<PathBuf> {
        LocalFileCollaborationStorage::repair(self, document, reason)
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

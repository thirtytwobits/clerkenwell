//! Durable single-authority storage for collaborative documents.
//!
//! One envelope per document holds a checksummed checkpoint and a window of
//! retained operations. A commit imports an update into the accepted replica,
//! judges it against the plan's field policies, validates the materialised
//! document, and replaces the envelope only if it is still the one the commit
//! read. Corruption is reported, never read as an empty document, and recovery
//! preserves evidence and records an audit trail.
//!
//! The service builds and judges every envelope. A storage port keeps each
//! document's envelope as bytes and replaces or removes them only from the
//! version a writer read; the local file port does so under a per-document
//! lock with an atomic durable write. A store set can hold stores over any
//! mix of ports.
//!
//! A service holds each document it reads, with the replica and materialised
//! document built from it, and writes every change through to the port. It
//! serves what it holds while the port's stamp vouches that the stored bytes
//! are unchanged, and otherwise reads them again. Recovery reads the stored
//! bytes.

mod envelope;
mod error;
mod residency;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clerkenwell_doc::{CollaborationReplica, CollaborationReplicaError};
use clerkenwell_events::{ChangeData, ChangeEvent, ChangeFeed, ChangeKind};
use clerkenwell_schema::GeneratedCollaborationEntitySpec;
pub use error::{StoreError, StoreErrorKind, StoreResult};
use fs2::FileExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use envelope::{CollaborationOperation, DurableCollaborationEnvelope};
use residency::{Residency, Resident};

/// The envelope format this store reads and writes. An envelope in an
/// earlier format is refused until [`CollaborationService::upgrade_envelope`]
/// rewrites it.
pub const ENVELOPE_VERSION: u32 = 3;

/// The fields an earlier format kept that the current one does not. Format 1
/// recorded where the application rendered the document and whether it had
/// published it; formats 1 and 2 recorded where an unfinished move took it
/// from.
fn retired_fields(envelope_version: u64) -> Option<&'static [&'static str]> {
    match envelope_version {
        1 => Some(&[
            "relative_path",
            "publication_pending",
            "pending_rename_from",
        ]),
        2 => Some(&["pending_rename_from"]),
        _ => None,
    }
}

/// The fields an earlier envelope format kept that the current one does not,
/// as an upgrade removed them.
pub type RetiredEnvelopeFields = serde_json::Map<String, Value>;

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

/// A stored document described without its accepted state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationDocumentSummary {
    pub document: CollaborationDocumentId,
    /// The format its envelope is kept in. A document in a format earlier
    /// than [`ENVELOPE_VERSION`] is described, but its content is neither
    /// read nor written until [`CollaborationService::upgrade_envelope`]
    /// rewrites it.
    pub envelope_version: u32,
    pub schema_version: u32,
    pub generation: u64,
    pub etag: String,
    pub checkpoint_sha256: String,
    pub checkpoint_bytes: usize,
    pub checkpoint_sequence: u64,
    pub compacted_through_sequence: u64,
    /// The sequences of the first and last operations kept past the
    /// checkpoint, when any are.
    pub retained_sequences: Option<(u64, u64)>,
    pub retained_count: usize,
}

impl CollaborationDocumentSummary {
    fn of(envelope: &DurableCollaborationEnvelope) -> Self {
        let retained = &envelope.retained_operations;
        Self {
            document: CollaborationDocumentId::new(
                envelope.entity.clone(),
                envelope.resource_id.clone(),
            ),
            envelope_version: envelope.envelope_version,
            schema_version: envelope.schema_version,
            generation: envelope.generation,
            etag: envelope.etag(),
            checkpoint_sha256: envelope.checkpoint_sha256.clone(),
            checkpoint_bytes: envelope.checkpoint_bytes,
            checkpoint_sequence: envelope.checkpoint_sequence,
            compacted_through_sequence: envelope.compacted_through_sequence,
            retained_sequences: retained
                .first()
                .zip(retained.last())
                .map(|(first, last)| (first.sequence, last.sequence)),
            retained_count: retained.len(),
        }
    }
}

/// A document as one read found it: what it is, and its accepted state
/// materialised, both of one stored version.
#[derive(Debug, Clone, PartialEq)]
pub struct CollaborationDocumentRead {
    pub summary: CollaborationDocumentSummary,
    pub document: Value,
}

/// One operation to commit over the envelope the service read.
#[derive(Debug, Clone)]
struct CollaborationCommit {
    document: CollaborationDocumentId,
    schema_version: u32,
    operation_id: String,
    imported_update: Vec<u8>,
    has_new_operations: bool,
    accepted_update: Vec<u8>,
    /// The accepted frontiers before and after the commit, when the caller
    /// read them.
    frontier_before: Option<String>,
    frontier_after: Option<String>,
}

#[derive(Debug, Clone)]
enum CollaborationCommitOutcome {
    Accepted(DurableCollaborationEnvelope),
    Duplicate(DurableCollaborationEnvelope),
    /// Another writer replaced the envelope after it was read.
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationExchangeMode {
    Incremental,
    Bootstrap,
}

/// A document's accepted state as one peer receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationAuthoringState {
    pub schema_version: u32,
    pub accepted_frontier_base64: String,
    /// The accepted operations the peer lacks: those after the frontier it
    /// holds, or every one when it holds none this document can place.
    pub update_base64: String,
    /// Hashes the whole stored checkpoint, whatever the update carries.
    pub etag: String,
}

#[derive(Debug, Clone)]
pub struct CollaborationImportRequest {
    pub document: CollaborationDocumentId,
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
    /// The accepted operations the importer lacks: those beyond its base
    /// frontier and the update it sent.
    pub missing_update_base64: String,
    pub etag: String,
    pub materialized: Value,
    pub generation: u64,
    pub peer_id: String,
    pub oplog_version: String,
    pub state_frontiers: String,
    pub update_bytes: usize,
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
    /// Documents the services sharing these counters hold in memory.
    pub resident_documents: u64,
    /// The accepted update bytes of the documents held in memory.
    pub resident_checkpoint_bytes: u64,
}

#[derive(Debug, Default)]
struct CollaborationRuntimeCounters {
    duplicate_imports: AtomicU64,
    dependency_blocks: AtomicU64,
    resync_requirements: AtomicU64,
    materialisation_failures: AtomicU64,
    commit_contention: AtomicU64,
    resident_documents: AtomicU64,
    resident_checkpoint_bytes: AtomicU64,
}

impl CollaborationRuntimeCounters {
    fn snapshot(&self) -> CollaborationRuntimeCountersSnapshot {
        CollaborationRuntimeCountersSnapshot {
            duplicate_imports: self.duplicate_imports.load(Ordering::Relaxed),
            dependency_blocks: self.dependency_blocks.load(Ordering::Relaxed),
            resync_requirements: self.resync_requirements.load(Ordering::Relaxed),
            materialisation_failures: self.materialisation_failures.load(Ordering::Relaxed),
            commit_contention: self.commit_contention.load(Ordering::Relaxed),
            resident_documents: self.resident_documents.load(Ordering::Relaxed),
            resident_checkpoint_bytes: self.resident_checkpoint_bytes.load(Ordering::Relaxed),
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

    /// A token, taken without reading the bytes kept at `source`, that differs
    /// from every earlier token for `source` once those bytes change. `None`
    /// when the port cannot vouch for such a token now; the reader then reads
    /// the bytes.
    fn stamp(&self, source: &str) -> StoreResult<Option<String>>;

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
    /// `label`, and returns where, as the port names places.
    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<String>;

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()>;

    /// Every recorded recovery request, oldest first.
    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>>;
}

/// How a service commits imports: the history each envelope keeps, and how
/// it tries again when another writer commits first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitPolicy {
    /// Operations an envelope keeps past its checkpoint. An import retried
    /// while its operation is kept is recognised as a duplicate.
    pub retained_operations: usize,
    /// Commits an import attempts before it is refused as contended.
    pub attempts: NonZeroU32,
    /// The bound on the pause before an import's second attempt. Each later
    /// bound doubles, and each pause is a random fraction of its bound.
    pub backoff: Duration,
}

/// Entity-neutral collaboration transaction service.
///
/// Generated plans define document layout. Resource adapters provide only
/// their seed document and semantic validator.
#[derive(Debug, Clone)]
pub struct CollaborationService {
    storage: Arc<dyn CollaborationStoragePort>,
    plans: &'static [GeneratedCollaborationEntitySpec],
    policy: CommitPolicy,
    counters: Arc<CollaborationRuntimeCounters>,
    changes: Arc<ChangeFeed>,
    source: Arc<str>,
    residency: Arc<Residency>,
    #[cfg(test)]
    faults: CollaborationFaults,
}

/// The stores an application keeps its documents in, each under a name. The
/// stores in a set share its plans, commit policy, counters and change feed,
/// and each names itself as the source of the changes it announces. The
/// application registers each store's root and routes each document to the
/// store that holds it.
#[derive(Debug, Clone)]
pub struct CollaborationStores {
    plans: &'static [GeneratedCollaborationEntitySpec],
    policy: CommitPolicy,
    counters: Arc<CollaborationRuntimeCounters>,
    changes: Arc<ChangeFeed>,
    stores: Arc<RwLock<BTreeMap<String, CollaborationService>>>,
}

fn registered(name: &str) -> StoreError {
    StoreError::conflict(format!(
        "A collaboration store is already registered as {name:?}."
    ))
    .with_data(serde_json::json!({
        "code": "collaboration_store_registered",
        "store": name,
    }))
}

impl CollaborationStores {
    /// A set, holding no store yet, for documents of `plans` committed under
    /// `policy`.
    pub fn new(plans: &'static [GeneratedCollaborationEntitySpec], policy: CommitPolicy) -> Self {
        Self {
            plans,
            policy,
            counters: Arc::new(CollaborationRuntimeCounters::default()),
            changes: Arc::new(ChangeFeed::default()),
            stores: Arc::default(),
        }
    }

    /// Registers `storage` as the store named `name`, and refuses a name the
    /// set already holds.
    pub fn register(
        &self,
        name: &str,
        storage: impl CollaborationStoragePort + 'static,
    ) -> StoreResult<CollaborationService> {
        let mut stores = self
            .stores
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if stores.contains_key(name) {
            return Err(registered(name));
        }
        let service = self.service(name, Arc::new(storage));
        stores.insert(name.to_string(), service.clone());
        Ok(service)
    }

    /// The store registered as `name`, or the storage `storage` builds
    /// registered under it when the set holds none.
    pub fn store_or_register<P: CollaborationStoragePort + 'static>(
        &self,
        name: &str,
        storage: impl FnOnce() -> P,
    ) -> CollaborationService {
        if let Some(existing) = self.store(name) {
            return existing;
        }
        let mut stores = self
            .stores
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stores
            .entry(name.to_string())
            .or_insert_with(|| self.service(name, Arc::new(storage())))
            .clone()
    }

    fn service(
        &self,
        name: &str,
        storage: Arc<dyn CollaborationStoragePort>,
    ) -> CollaborationService {
        CollaborationService::within(
            storage,
            self.plans,
            self.policy,
            self.counters.clone(),
            self.changes.clone(),
            name.to_string(),
        )
    }

    /// The store registered as `name`.
    pub fn store(&self, name: &str) -> Option<CollaborationService> {
        self.read().get(name).cloned()
    }

    /// Removes the store registered as `name` from the set, leaving its
    /// envelopes where they are. Returns whether the set held it.
    pub fn forget(&self, name: &str) -> bool {
        self.stores
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(name)
            .is_some()
    }

    /// Every store in the set, by name.
    pub fn stores(&self) -> Vec<(String, CollaborationService)> {
        self.read()
            .iter()
            .map(|(name, service)| (name.clone(), service.clone()))
            .collect()
    }

    /// The feed every store in the set announces its changes on.
    pub fn changes(&self) -> &ChangeFeed {
        &self.changes
    }

    /// What the stores in the set have counted between them.
    pub fn counters(&self) -> CollaborationRuntimeCountersSnapshot {
        self.counters.snapshot()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, CollaborationService>> {
        self.stores
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl CollaborationService {
    /// A service for documents of `plans` whose envelopes live in files under
    /// `root`.
    pub fn new(
        root: &Path,
        plans: &'static [GeneratedCollaborationEntitySpec],
        policy: CommitPolicy,
    ) -> Self {
        let storage = LocalFileCollaborationStorage::new(root);
        #[cfg(test)]
        let faults = storage.faults.clone();
        let service = Self::with_storage(storage, plans, policy, root.display().to_string());
        #[cfg(test)]
        let service = Self { faults, ..service };
        service
    }

    /// A service for documents of `plans` committing through any
    /// implementation of the storage port, whose changes name `source` as
    /// their source.
    pub fn with_storage(
        storage: impl CollaborationStoragePort + 'static,
        plans: &'static [GeneratedCollaborationEntitySpec],
        policy: CommitPolicy,
        source: impl Into<String>,
    ) -> Self {
        Self::within(
            Arc::new(storage),
            plans,
            policy,
            Arc::new(CollaborationRuntimeCounters::default()),
            Arc::new(ChangeFeed::default()),
            source.into(),
        )
    }

    /// A service sharing `counters` and `changes`, as the stores of one set do.
    fn within(
        storage: Arc<dyn CollaborationStoragePort>,
        plans: &'static [GeneratedCollaborationEntitySpec],
        policy: CommitPolicy,
        counters: Arc<CollaborationRuntimeCounters>,
        changes: Arc<ChangeFeed>,
        source: String,
    ) -> Self {
        Self {
            storage,
            plans,
            policy,
            residency: Arc::new(Residency::new(counters.clone())),
            counters,
            changes,
            source: source.into(),
            #[cfg(test)]
            faults: CollaborationFaults::default(),
        }
    }

    pub fn storage(&self) -> &dyn CollaborationStoragePort {
        self.storage.as_ref()
    }

    /// The feed this service announces each change to a document's accepted
    /// state on: a commit, a repair or a deletion.
    pub fn changes(&self) -> &ChangeFeed {
        &self.changes
    }

    fn announce(
        &self,
        kind: ChangeKind,
        generation: Option<u64>,
        data: impl FnOnce() -> ChangeData,
    ) {
        if !self.changes.is_heard() {
            return;
        }
        self.changes.announce(&ChangeEvent::new(
            Uuid::new_v4().to_string(),
            self.source.as_ref(),
            kind,
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            generation,
            data(),
        ));
    }

    /// The accepted frontier `envelope` holds, when its plan can read it.
    fn accepted_frontier(&self, envelope: &DurableCollaborationEnvelope) -> Option<String> {
        let plan = self
            .plans
            .iter()
            .find(|plan| plan.name == envelope.entity)?;
        let document =
            CollaborationDocumentId::new(envelope.entity.clone(), envelope.resource_id.clone());
        // The held replica, when it is of this state, serves the readers the
        // announcement brings as well.
        let held = self
            .residency
            .get(&self.storage.source(&document))
            .filter(|held| {
                held.envelope.checkpoint_sha256 == envelope.checkpoint_sha256
                    && held.envelope.schema_version == envelope.schema_version
            });
        match held {
            Some(held) => Self::replica(plan, &document, &held).ok(),
            None => load_authoring_document(plan, &document, envelope)
                .ok()
                .map(Arc::new),
        }
        .map(|authoring| authoring.accepted_frontier_base64())
    }

    /// `document`'s validated envelope, or `None` when it has none.
    pub(crate) fn load(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<DurableCollaborationEnvelope>> {
        Ok(self
            .read_current(document)?
            .map(|read| read.envelope.clone()))
    }

    /// What `document` is, or `None` when none is stored.
    pub fn summary(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<CollaborationDocumentSummary>> {
        Ok(self
            .read_envelope(document)?
            .map(|read| CollaborationDocumentSummary::of(&read.envelope)))
    }

    /// What every stored document of `entity` is, in resource order.
    pub fn summaries(&self, entity: &str) -> StoreResult<Vec<CollaborationDocumentSummary>> {
        let mut summaries = Vec::new();
        for source in self.storage.sources(entity)? {
            if let Some(read) = self.resident(&source, None)? {
                summaries.push(CollaborationDocumentSummary::of(&read.envelope));
            }
        }
        summaries.sort_by(|left, right| left.document.resource_id.cmp(&right.document.resource_id));
        Ok(summaries)
    }

    /// `document`'s accepted state materialised with what it is, or `None`
    /// when none is stored.
    pub fn read_document(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<CollaborationDocumentRead>> {
        let Some(resident) = self.read_current(document)? else {
            return Ok(None);
        };
        Ok(Some(CollaborationDocumentRead {
            document: (*Self::materialized(plan, document, &resident)?).clone(),
            summary: CollaborationDocumentSummary::of(&resident.envelope),
        }))
    }

    /// Stores `envelope` for a document that has none, and returns the
    /// document's envelope either way.
    #[cfg(test)]
    pub(crate) fn install(
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
                self.announce(ChangeKind::Committed, Some(envelope.generation), || {
                    ChangeData {
                        entity: document.entity.clone(),
                        resource_id: document.resource_id.clone(),
                        etag_before: None,
                        etag_after: Some(envelope.etag()),
                        frontier_before: None,
                        frontier_after: self.accepted_frontier(&envelope),
                        operation_id: None,
                    }
                });
                return Ok(envelope);
            }
        }
    }

    /// `document` as held, refusing one whose envelope is in an earlier
    /// format: its content is read and written only once it is upgraded.
    fn read_current(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<Arc<Resident>>> {
        let read = self.read_envelope(document)?;
        if let Some(read) = &read {
            require_current(document, &read.envelope)?;
        }
        Ok(read)
    }

    fn read_envelope(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<Arc<Resident>>> {
        self.resident(&self.storage.source(document), Some(document))
    }

    /// The document kept at `source` as this service holds it. What it holds
    /// is served while the storage port's stamp vouches for it, and kept when
    /// the bytes read in its place carry its version; anything else is read,
    /// judged as `document`'s envelope (or as the envelope it names) and held.
    fn resident(
        &self,
        source: &str,
        document: Option<&CollaborationDocumentId>,
    ) -> StoreResult<Option<Arc<Resident>>> {
        let held = self.residency.get(source);
        // Taken before the read, so a write between the two leaves a stamp
        // that no longer vouches for what was read.
        let stamp = self.storage.stamp(source)?;
        if let Some(held) = held
            .as_ref()
            .filter(|held| held.vouched_for_by(stamp.as_deref()))
        {
            return Ok(Some(held.clone()));
        }
        let Some(stored) = self.storage.read(source)? else {
            self.residency.forget(source);
            return Ok(None);
        };
        if let Some(held) = held.filter(|held| held.version == stored.version) {
            held.restamp(stamp);
            return Ok(Some(held));
        }
        let envelope = parse_envelope(document, source, &stored.bytes)?;
        match document {
            Some(document) => validate_envelope(document, &envelope)?,
            None => validate_envelope(
                &CollaborationDocumentId::new(
                    envelope.entity.clone(),
                    envelope.resource_id.clone(),
                ),
                &envelope,
            )?,
        }
        let resident = Arc::new(Resident::new(stored.version, stamp, envelope));
        self.residency.hold(source, resident.clone());
        Ok(Some(resident))
    }

    /// The replica of the document `resident` holds, built once.
    fn replica(
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        resident: &Resident,
    ) -> StoreResult<Arc<CollaborationReplica>> {
        resident.replica(|envelope| load_authoring_document(plan, document, envelope))
    }

    /// The document `resident` holds, materialised once.
    fn materialized(
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        resident: &Resident,
    ) -> StoreResult<Arc<Value>> {
        let replica = Self::replica(plan, document, resident)?;
        resident.materialized(|| {
            replica
                .materialized_document(&resident.envelope.etag())
                .map_err(|error| replica_error(document, error))
        })
    }

    /// Stores `envelope` as `document`'s if the stored version is still
    /// `expected`, returning the new version, or `None` when it has moved.
    /// The service holds what it stored.
    fn swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        envelope: &DurableCollaborationEnvelope,
    ) -> StoreResult<Option<String>> {
        validate_envelope(document, envelope)?;
        let bytes = serde_json::to_vec_pretty(envelope)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        let stored = self.storage.compare_and_swap(document, expected, &bytes)?;
        if let Some(version) = &stored {
            self.residency.hold(
                &self.storage.source(document),
                Arc::new(Resident::new(version.clone(), None, envelope.clone())),
            );
        }
        Ok(stored)
    }

    /// Removes `document`'s envelope if its stored version is still
    /// `expected`, and stops holding it.
    fn remove(&self, document: &CollaborationDocumentId, expected: &str) -> StoreResult<bool> {
        let removed = self.storage.compare_and_remove(document, expected)?;
        if removed {
            self.residency.forget(&self.storage.source(document));
        }
        Ok(removed)
    }

    /// Appends `commit`'s operation to the envelope the service read, or
    /// reports it as a duplicate of one that envelope already holds.
    fn commit(
        &self,
        current: Option<&Resident>,
        commit: CollaborationCommit,
    ) -> StoreResult<CollaborationCommitOutcome> {
        let document = commit.document.clone();
        let operation_id = commit.operation_id.clone();
        if let Some(Resident { envelope, .. }) = current {
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
        let envelope = self.next_envelope(current_envelope, &document, &commit);
        let expected = current.map(|read| read.version.as_str());
        Ok(match self.swap(&document, expected, &envelope)? {
            Some(_) => {
                self.announce(ChangeKind::Committed, Some(envelope.generation), || {
                    ChangeData {
                        entity: document.entity.clone(),
                        resource_id: document.resource_id.clone(),
                        etag_before: current_envelope.map(DurableCollaborationEnvelope::etag),
                        etag_after: Some(envelope.etag()),
                        frontier_before: commit.frontier_before,
                        frontier_after: commit
                            .frontier_after
                            .or_else(|| self.accepted_frontier(&envelope)),
                        operation_id: Some(operation_id.clone()),
                    }
                });
                CollaborationCommitOutcome::Accepted(envelope)
            }
            None => CollaborationCommitOutcome::Stale,
        })
    }

    /// The envelope `document` has once `commit` is accepted over `current`,
    /// its history kept within the service's commit policy.
    fn next_envelope(
        &self,
        current: Option<&DurableCollaborationEnvelope>,
        document: &CollaborationDocumentId,
        commit: &CollaborationCommit,
    ) -> DurableCollaborationEnvelope {
        let sequence = current.map_or(1, |state| state.checkpoint_sequence.saturating_add(1));
        let schema_changed =
            current.is_some_and(|state| state.schema_version != commit.schema_version);
        let mut retained_operations = if schema_changed {
            Vec::new()
        } else {
            current.map_or_else(Vec::new, |state| state.retained_operations.clone())
        };
        retained_operations.push(CollaborationOperation::from_update(
            commit.operation_id.clone(),
            sequence,
            commit.schema_version,
            &commit.imported_update,
        ));
        let kept = self.policy.retained_operations;
        if retained_operations.len() > kept {
            retained_operations.drain(0..retained_operations.len() - kept);
        }
        let retained_from = retained_operations
            .first()
            .map_or(sequence, |operation| operation.sequence);
        DurableCollaborationEnvelope {
            envelope_version: ENVELOPE_VERSION,
            entity: document.entity.clone(),
            resource_id: document.resource_id.clone(),
            schema_version: commit.schema_version,
            generation: current
                .map_or(0, |state| state.generation)
                .saturating_add(1),
            checkpoint_sequence: sequence,
            compacted_through_sequence: retained_from.saturating_sub(1),
            checkpoint_update_base64: BASE64.encode(&commit.accepted_update),
            checkpoint_sha256: sha256_hex(&commit.accepted_update),
            checkpoint_bytes: commit.accepted_update.len(),
            retained_operations,
        }
    }

    pub fn detail(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<Value>> {
        let Some(resident) = self.read_current(document)? else {
            return Ok(None);
        };
        Ok(Some(
            (*Self::materialized(plan, document, &resident)?).clone(),
        ))
    }

    /// The accepted state for a peer that holds every operation up to
    /// `held_frontier_base64`, or nothing when it is `None`.
    pub fn authoring_state(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        held_frontier_base64: Option<&str>,
    ) -> StoreResult<CollaborationAuthoringState> {
        let resident = self.read_current(document)?.ok_or_else(|| {
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
        // Building the replica checks the checkpoint the whole update is.
        let authoring = Self::replica(plan, document, &resident)?;
        let envelope = &resident.envelope;
        let lacking = match held_frontier_base64 {
            None => envelope.checkpoint_update_base64.clone(),
            Some(held) => match authoring.export_incremental_update_base64(held) {
                Ok(lacking) => lacking,
                // A frontier from another history places nothing the peer holds.
                Err(CollaborationReplicaError::UnknownFrontier) => {
                    envelope.checkpoint_update_base64.clone()
                }
                Err(error) => return Err(replica_error(document, error)),
            },
        };
        Ok(CollaborationAuthoringState {
            schema_version: envelope.schema_version,
            accepted_frontier_base64: authoring.accepted_frontier_base64(),
            update_base64: lacking,
            etag: envelope.etag(),
        })
    }

    pub fn import<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        request: CollaborationImportRequest,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<CollaborationImportResult, E> {
        self.import_internal(plan, request, validate)
    }

    fn import_internal<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        request: CollaborationImportRequest,
        validate: impl Fn(&Value) -> Result<(), E>,
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
                StoreError::invalid_request(format!("Invalid update_base64: {error}")).with_data(
                    serde_json::json!({
                        "code": "invalid_collaboration_update",
                        "entity": request.document.entity,
                        "resource_id": request.document.resource_id,
                    }),
                )
            })?;

        for attempt in 0..self.policy.attempts.get() {
            if attempt > 0 {
                self.pause_before_attempt(attempt);
            }
            let read = self.read_current(&request.document)?;
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
                    let authoring = CollaborationReplica::from_versioned_update_base64(
                        plan,
                        request.schema_version,
                        &request.update_base64,
                    )
                    .map_err(|error| {
                        self.record_replica_failure(&error);
                        replica_error(&request.document, error)
                    })?;
                    let accepted_update = BASE64
                        .decode(
                            authoring
                                .export_update_base64()
                                .map_err(|error| replica_error(&request.document, error))?,
                        )
                        .map_err(|error| StoreError::internal(error.to_string()))?;
                    DurableCollaborationEnvelope {
                        envelope_version: ENVELOPE_VERSION,
                        entity: request.document.entity.clone(),
                        resource_id: request.document.resource_id.clone(),
                        schema_version: request.schema_version,
                        generation: 0,
                        checkpoint_sequence: 0,
                        compacted_through_sequence: 0,
                        checkpoint_update_base64: BASE64.encode(&accepted_update),
                        checkpoint_sha256: sha256_hex(&accepted_update),
                        checkpoint_bytes: accepted_update.len(),
                        retained_operations: Vec::new(),
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
            let mut authoring = load_authoring_document(plan, &request.document, &current)?;
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
                        .map_err(|error| replica_error(&request.document, error))?;
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
                        self.record_replica_failure(&error);
                        replica_error(&request.document, error)
                    })?;
            }
            if request.exchange_mode == CollaborationExchangeMode::Incremental {
                let conflicts = authoring
                    .policy_conflicts_for_incremental_update(
                        &request.base_frontier_base64,
                        request.schema_version,
                        &request.update_base64,
                    )
                    .map_err(|error| {
                        self.record_replica_failure(&error);
                        replica_error(&request.document, error)
                    })?;
                if !conflicts.is_empty() {
                    let current_document = authoring
                        .materialized_document(&current_etag)
                        .map_err(|error| replica_error(&request.document, error))?;
                    // The refused client rebases its edit on what it lacks and sends it again.
                    let missing_update_base64 = authoring
                        .missing_update_base64(
                            Some(&request.base_frontier_base64),
                            &request.update_base64,
                        )
                        .map_err(|error| replica_error(&request.document, error))?;
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
                        "accepted_frontier_base64": authoring.accepted_frontier_base64(),
                        "missing_update_base64": missing_update_base64,
                        "draft_retained": true,
                    }))
                    .into());
                }
            }
            let before_import = authoring.accepted_frontier_base64();
            authoring
                .adopt_versioned_update_base64(request.schema_version, &request.update_base64)
                .map_err(|error| {
                    self.record_replica_failure(&error);
                    replica_error(&request.document, error)
                })?;
            #[cfg(test)]
            self.faults
                .fail_if(CollaborationFaultPoint::CandidateImported)?;
            let accepted_update = BASE64
                .decode(
                    authoring
                        .export_update_base64()
                        .map_err(|error| replica_error(&request.document, error))?,
                )
                .map_err(|error| StoreError::internal(error.to_string()))?;
            let etag = collaboration_etag(&accepted_update);
            let materialized = authoring.materialized_document(&etag).map_err(|error| {
                self.counters
                    .materialisation_failures
                    .fetch_add(1, Ordering::Relaxed);
                replica_error(&request.document, error)
            })?;
            #[cfg(test)]
            self.faults.fail_if(CollaborationFaultPoint::Materialised)?;
            validate(&materialized)?;
            #[cfg(test)]
            self.faults.fail_if(CollaborationFaultPoint::Validated)?;
            let outcome = self.commit(
                read.as_deref(),
                CollaborationCommit {
                    document: request.document.clone(),
                    schema_version: request.schema_version,
                    operation_id: request.operation_id.clone(),
                    imported_update: imported_update.clone(),
                    has_new_operations: before_import != authoring.accepted_frontier_base64(),
                    accepted_update: accepted_update.clone(),
                    frontier_before: Some(before_import.clone()),
                    frontier_after: Some(authoring.accepted_frontier_base64()),
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
            // Report the durable checkpoint, including on retries. Exporting the
            // same history again need not reproduce its original snapshot bytes.
            let durable_update = accepted.checkpoint_update(&request.document)?;
            let (authoring, materialized) = if durable_update == accepted_update {
                (authoring, materialized)
            } else {
                let durable = load_authoring_document(plan, &request.document, &accepted)?;
                let value = durable
                    .materialized_document(&collaboration_etag(&durable_update))
                    .map_err(|error| replica_error(&request.document, error))?;
                (durable, value)
            };
            let accepted_update = durable_update;
            let authoring = Arc::new(authoring);
            if let Some(held) = self
                .residency
                .get(&self.storage.source(&request.document))
                .filter(|held| {
                    held.envelope.generation == accepted.generation
                        && held.envelope.checkpoint_sha256 == accepted.checkpoint_sha256
                })
            {
                held.offer(authoring.clone(), materialized.clone());
            }
            let missing_update_base64 = authoring
                .missing_update_base64(
                    (request.exchange_mode == CollaborationExchangeMode::Incremental)
                        .then_some(request.base_frontier_base64.as_str()),
                    &request.update_base64,
                )
                .map_err(|error| replica_error(&request.document, error))?;
            let debug = authoring.debug();
            return Ok(CollaborationImportResult {
                operation_id: request.operation_id,
                duplicate,
                schema_version: accepted.schema_version,
                accepted_frontier_base64: authoring.accepted_frontier_base64(),
                missing_update_base64,
                etag: collaboration_etag(&accepted_update),
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

    /// Sleeps a random fraction of the bound before commit `attempt`, which
    /// doubles from the policy's backoff with each attempt after the second.
    fn pause_before_attempt(&self, attempt: u32) {
        let bound = self
            .policy
            .backoff
            .saturating_mul(1 << (attempt - 1).min(16));
        let (random, _) = Uuid::new_v4().as_u64_pair();
        std::thread::sleep(bound.mul_f64(random as f64 / u64::MAX as f64));
    }

    /// Rewrites `document`'s envelope in the current format when it is kept
    /// in an earlier one, and returns the fields the earlier format kept that
    /// the current one does not; `None` when it is already current. The
    /// document, its generation and its history are unchanged.
    pub fn upgrade_envelope(
        &self,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<RetiredEnvelopeFields>> {
        loop {
            let stored = self.stored(document)?;
            let Some((envelope, removed)) =
                upgraded_envelope(document, &self.storage.source(document), &stored.bytes)?
            else {
                return Ok(None);
            };
            if self
                .swap(document, Some(&stored.version), &envelope)?
                .is_some()
            {
                return Ok(Some(removed));
            }
        }
    }

    /// Removes `document`'s envelope, whatever it holds.
    pub fn delete(&self, document: &CollaborationDocumentId) -> StoreResult<()> {
        let source = self.storage.source(document);
        while let Some(stored) = self.storage.read(&source)? {
            if self.remove(document, &stored.version)? {
                let deleted = parse_envelope(Some(document), &source, &stored.bytes).ok();
                self.announce(
                    ChangeKind::Deleted,
                    deleted.as_ref().map(|envelope| envelope.generation),
                    || ChangeData {
                        entity: document.entity.clone(),
                        resource_id: document.resource_id.clone(),
                        etag_before: deleted.as_ref().map(DurableCollaborationEnvelope::etag),
                        etag_after: None,
                        frontier_before: None,
                        frontier_after: None,
                        operation_id: None,
                    },
                );
                break;
            }
        }
        Ok(())
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
        let Ok(envelope) = parse_envelope(None, source, bytes) else {
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
                migration_required: false,
                valid: false,
                failure_code: Some("invalid_envelope_json".to_string()),
            };
        };
        let document =
            CollaborationDocumentId::new(envelope.entity.clone(), envelope.resource_id.clone());
        let failure_code = match validate_envelope(&document, &envelope) {
            Err(_) => Some("collaboration_state_corrupt"),
            Ok(()) if envelope.envelope_version < ENVELOPE_VERSION => {
                Some("collaboration_envelope_upgrade_required")
            }
            Ok(()) => None,
        };
        inspection_from_envelope(
            self.plans,
            envelope,
            failure_code.is_none(),
            failure_code.map(str::to_string),
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
    ) -> StoreResult<String> {
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
    pub fn repair(&self, document: &CollaborationDocumentId, reason: &str) -> StoreResult<String> {
        self.audit_recovery_request(
            CollaborationRecoveryAction::Repair,
            document,
            Some(reason),
            false,
        )?;
        let stored = self.stored(document)?;
        let mut envelope = parse_envelope(
            Some(document),
            &self.storage.source(document),
            &stored.bytes,
        )
        .map_err(|_| {
            corrupt_state(
                document,
                "the envelope JSON cannot be repaired automatically",
            )
        })?;
        require_current(document, &envelope)?;
        let _ = envelope.checkpoint_update(document)?;
        let evidence = self
            .storage
            .preserve(document, &evidence_label(reason), &stored.bytes)?;
        let etag_before = envelope.etag();
        let frontier_before = self.accepted_frontier(&envelope);
        envelope.retained_operations.clear();
        envelope.compacted_through_sequence = envelope.checkpoint_sequence;
        envelope.generation = envelope.generation.saturating_add(1);
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
        self.announce(ChangeKind::Committed, Some(envelope.generation), || {
            ChangeData {
                entity: document.entity.clone(),
                resource_id: document.resource_id.clone(),
                etag_before: Some(etag_before),
                etag_after: Some(envelope.etag()),
                frontier_before,
                frontier_after: self.accepted_frontier(&envelope),
                operation_id: None,
            }
        });
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

    fn record_replica_failure(&self, error: &CollaborationReplicaError) {
        match error {
            CollaborationReplicaError::MissingDependency => {
                self.counters
                    .dependency_blocks
                    .fetch_add(1, Ordering::Relaxed);
            }
            CollaborationReplicaError::UnknownFrontier => {
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
        seed: &Value,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<CollaborationDocumentSummary, E> {
        if let Some(current) = self.load(document)? {
            return Ok(CollaborationDocumentSummary::of(&current));
        }
        let authoring = CollaborationReplica::from_document(plan, seed)
            .map_err(|error| replica_error(document, error))?;
        let accepted_update = BASE64
            .decode(
                authoring
                    .export_update_base64()
                    .map_err(|error| replica_error(document, error))?,
            )
            .map_err(|error| StoreError::internal(error.to_string()))?;
        let etag = collaboration_etag(&accepted_update);
        let materialized = authoring
            .materialized_document(&etag)
            .map_err(|error| replica_error(document, error))?;
        validate(&materialized)?;
        Ok(CollaborationDocumentSummary::of(&self.seed(
            plan,
            document,
            accepted_update,
        )?))
    }

    pub fn bootstrap_update<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        schema_version: u32,
        update_base64: &str,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<CollaborationDocumentSummary, E> {
        if let Some(current) = self.load(document)? {
            return Ok(CollaborationDocumentSummary::of(&current));
        }
        let authoring =
            CollaborationReplica::from_versioned_update_base64(plan, schema_version, update_base64)
                .map_err(|error| replica_error(document, error))?;
        let accepted_update = BASE64
            .decode(update_base64)
            .map_err(|error| StoreError::invalid_request(error.to_string()))?;
        let etag = collaboration_etag(&accepted_update);
        let materialized = authoring
            .materialized_document(&etag)
            .map_err(|error| replica_error(document, error))?;
        validate(&materialized)?;
        Ok(CollaborationDocumentSummary::of(&self.seed(
            plan,
            document,
            accepted_update,
        )?))
    }

    pub fn migrate<E: From<StoreError>>(
        &self,
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &CollaborationDocumentId,
        from_schema_version: u32,
        migrated_update_base64: &str,
        validate: impl Fn(&Value) -> Result<(), E>,
    ) -> Result<CollaborationDocumentSummary, E> {
        let migrated = CollaborationReplica::from_versioned_update_base64(
            plan,
            plan.schema_version,
            migrated_update_base64,
        )
        .map_err(|error| replica_error(document, error))?;
        let accepted_update = BASE64
            .decode(migrated_update_base64)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        let materialized = migrated
            .materialized_document(&collaboration_etag(&accepted_update))
            .map_err(|error| replica_error(document, error))?;
        validate(&materialized)?;

        loop {
            let read = self
                .read_current(document)?
                .ok_or_else(|| missing_state(document))?;
            let current = &read.envelope;
            if current.schema_version == plan.schema_version {
                return Ok(CollaborationDocumentSummary::of(&read.envelope));
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
                    schema_version: plan.schema_version,
                    operation_id,
                    imported_update: accepted_update.clone(),
                    has_new_operations: true,
                    accepted_update: accepted_update.clone(),
                    frontier_before: None,
                    frontier_after: Some(migrated.accepted_frontier_base64()),
                },
            )? {
                CollaborationCommitOutcome::Accepted(migrated)
                | CollaborationCommitOutcome::Duplicate(migrated) => {
                    return Ok(CollaborationDocumentSummary::of(&migrated))
                }
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
                    schema_version: plan.schema_version,
                    operation_id: operation_id.clone(),
                    imported_update: accepted_update.clone(),
                    has_new_operations: true,
                    accepted_update: accepted_update.clone(),
                    frontier_before: None,
                    frontier_after: None,
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
) -> StoreResult<CollaborationReplica> {
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
    CollaborationReplica::from_versioned_update_base64(
        plan,
        envelope.schema_version,
        &update_base64,
    )
    .map_err(|error| replica_error(document, error))
}

fn collaboration_etag(accepted_update: &[u8]) -> String {
    checkpoint_etag(&sha256_hex(accepted_update))
}

fn checkpoint_etag(checkpoint_sha256: &str) -> String {
    format!("loro:{checkpoint_sha256}")
}

fn replica_error(
    document: &CollaborationDocumentId,
    error: CollaborationReplicaError,
) -> StoreError {
    match error {
        CollaborationReplicaError::UnknownFrontier => StoreError::conflict(format!(
            "The collaboration frontier for {}/{} is no longer available; resynchronise without discarding the retained draft.",
            document.entity, document.resource_id
        ))
        .with_data(serde_json::json!({
            "code": "collaboration_resync_required",
            "entity": document.entity,
            "resource_id": document.resource_id,
            "draft_retained": true,
        })),
        CollaborationReplicaError::MissingDependency => StoreError::conflict(format!(
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

/// How long a file must have gone unchanged before its metadata alone shows
/// any later change: a file timestamp can lag the clock by a timer tick, or by
/// a second on a coarse file system, so a rewrite within that lag can leave
/// the timestamp where it was.
pub(crate) const SETTLED_METADATA_AGE: Duration = Duration::from_secs(2);

/// When a file last changed, and a stamp of its metadata. The stamp differs
/// from every earlier stamp of the file once its bytes change, provided the
/// earlier one was taken at least [`SETTLED_METADATA_AGE`] after the file last
/// changed.
#[cfg(unix)]
pub(crate) fn metadata_stamp(metadata: &std::fs::Metadata) -> Option<(SystemTime, String)> {
    use std::os::unix::fs::MetadataExt;
    let changed = UNIX_EPOCH.checked_add(Duration::new(
        u64::try_from(metadata.ctime()).ok()?,
        u32::try_from(metadata.ctime_nsec()).ok()?,
    ))?;
    let stamp = format!(
        "{}:{}:{}:{}.{}:{}.{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    );
    Some((changed, stamp))
}

/// When a file last changed, and a stamp of its metadata. The stamp differs
/// from every earlier stamp of the file once its bytes change, provided the
/// earlier one was taken at least [`SETTLED_METADATA_AGE`] after the file last
/// changed.
#[cfg(not(unix))]
pub(crate) fn metadata_stamp(metadata: &std::fs::Metadata) -> Option<(SystemTime, String)> {
    let modified = metadata.modified().ok()?;
    let since_epoch = modified.duration_since(UNIX_EPOCH).ok()?;
    Some((
        modified,
        format!("{}:{}", metadata.len(), since_epoch.as_nanos()),
    ))
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

    fn stamp(&self, source: &str) -> StoreResult<Option<String>> {
        let metadata = match std::fs::metadata(source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StoreError::internal(format!(
                    "Could not inspect collaboration state {source}: {error}"
                )))
            }
        };
        Ok(metadata_stamp(&metadata).and_then(|(changed, stamp)| {
            SystemTime::now()
                .duration_since(changed)
                .is_ok_and(|age| age >= SETTLED_METADATA_AGE)
                .then_some(stamp)
        }))
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
    ) -> StoreResult<String> {
        let destination = self.root.join("quarantine").join(format!(
            "{}-{}-{}-{}.json",
            safe_entity_name(&document.entity),
            sha256_hex(document.resource_id.as_bytes()),
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
            label
        ));
        write_atomic_durable(&destination, bytes)?;
        Ok(destination.display().to_string())
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
        migration_required: expected_schema_version
            .is_some_and(|expected| expected != envelope.schema_version),
        valid,
        failure_code,
    }
}

/// The format version an envelope's bytes declare, read before the rest.
#[derive(Deserialize)]
struct DeclaredEnvelopeVersion {
    envelope_version: u32,
}

/// `bytes` as the envelope kept at `source`, which `document` names when it
/// is known. An envelope in an earlier format is reported as needing its
/// upgrade rather than as unreadable.
fn parse_envelope(
    document: Option<&CollaborationDocumentId>,
    source: &str,
    bytes: &[u8],
) -> StoreResult<DurableCollaborationEnvelope> {
    let unreadable = |error: serde_json::Error| match document {
        Some(document) => corrupt_state(
            document,
            format!("envelope {source} is invalid JSON: {error}"),
        ),
        None => StoreError::internal(format!(
            "Collaboration envelope {source} is invalid JSON: {error}"
        )),
    };
    let declared = serde_json::from_slice::<DeclaredEnvelopeVersion>(bytes)
        .map_err(&unreadable)?
        .envelope_version;
    let Some(retired) = retired_fields(u64::from(declared)) else {
        return serde_json::from_slice(bytes).map_err(unreadable);
    };
    // What an earlier format kept beyond the current one is left out; its
    // version says the envelope still needs its upgrade.
    let mut fields: serde_json::Map<String, Value> =
        serde_json::from_slice(bytes).map_err(&unreadable)?;
    for field in retired {
        fields.remove(*field);
    }
    serde_json::from_value(Value::Object(fields)).map_err(unreadable)
}

/// Refuses to read or write the content of an envelope kept in an earlier
/// format.
fn require_current(
    document: &CollaborationDocumentId,
    envelope: &DurableCollaborationEnvelope,
) -> StoreResult<()> {
    if envelope.envelope_version >= ENVELOPE_VERSION {
        return Ok(());
    }
    Err(StoreError::invalid_request(format!(
        "Collaboration document {}/{} is kept in envelope format {}; upgrade it to format {ENVELOPE_VERSION} first.",
        document.entity, document.resource_id, envelope.envelope_version
    ))
    .with_data(serde_json::json!({
        "code": "collaboration_envelope_upgrade_required",
        "entity": document.entity,
        "resource_id": document.resource_id,
        "actual": envelope.envelope_version,
        "expected": ENVELOPE_VERSION,
    })))
}

fn validate_envelope(
    document: &CollaborationDocumentId,
    envelope: &DurableCollaborationEnvelope,
) -> StoreResult<()> {
    if !(1..=ENVELOPE_VERSION).contains(&envelope.envelope_version) {
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

/// Stored envelope `bytes` rewritten in the current format, and the fields the
/// earlier format kept that the current one does not; `None` when they are
/// already current. For envelopes no service reads, such as a staged copy of
/// a store; [`CollaborationService::upgrade_envelope`] upgrades one a service
/// keeps.
pub fn upgrade_envelope_bytes(
    bytes: &[u8],
) -> StoreResult<Option<(Vec<u8>, RetiredEnvelopeFields)>> {
    let named = |field: &str| {
        serde_json::from_slice::<Value>(bytes)
            .ok()
            .and_then(|value| value.get(field)?.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let document = CollaborationDocumentId::new(named("entity"), named("resource_id"));
    let Some((envelope, removed)) = upgraded_envelope(&document, "the stored envelope", bytes)?
    else {
        return Ok(None);
    };
    validate_envelope(&document, &envelope)?;
    let bytes = serde_json::to_vec_pretty(&envelope)
        .map_err(|error| StoreError::internal(error.to_string()))?;
    Ok(Some((bytes, removed)))
}

/// An envelope rewritten in the current format, and the fields its earlier
/// format kept that the current one does not.
type UpgradedEnvelope = (DurableCollaborationEnvelope, RetiredEnvelopeFields);

/// `bytes`, kept at `source`, read as an envelope in the current format,
/// with the fields an earlier format kept that it does not; `None` when they
/// are already current.
fn upgraded_envelope(
    document: &CollaborationDocumentId,
    source: &str,
    bytes: &[u8],
) -> StoreResult<Option<UpgradedEnvelope>> {
    let unreadable = |error: serde_json::Error| {
        corrupt_state(
            document,
            format!("envelope {source} is invalid JSON: {error}"),
        )
    };
    let mut fields: serde_json::Map<String, Value> =
        serde_json::from_slice(bytes).map_err(unreadable)?;
    let version = fields
        .get("envelope_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| corrupt_state(document, "the envelope names no format version"))?;
    if version == u64::from(ENVELOPE_VERSION) {
        return Ok(None);
    }
    let Some(retired) = retired_fields(version) else {
        return Err(corrupt_state(
            document,
            format!("unsupported durable envelope version {version}"),
        ));
    };
    let removed: RetiredEnvelopeFields = retired
        .iter()
        .filter_map(|field| {
            fields
                .remove(*field)
                .map(|value| (field.to_string(), value))
        })
        .collect();
    fields.insert(
        "envelope_version".to_string(),
        Value::from(ENVELOPE_VERSION),
    );
    let envelope: DurableCollaborationEnvelope =
        serde_json::from_value(Value::Object(fields)).map_err(unreadable)?;
    Ok(Some((envelope, removed)))
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

pub(crate) fn write_atomic_durable(path: &Path, bytes: &[u8]) -> StoreResult<()> {
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

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

/// The stored envelope format, for Clerkenwell's own tests.
#[cfg(feature = "testing")]
pub mod testing {
    pub use crate::envelope::{CollaborationOperation, DurableCollaborationEnvelope};
    use crate::{CollaborationDocumentId, CollaborationService, StoreResult};

    /// `document`'s validated envelope, or `None` when it has none.
    pub fn load(
        service: &CollaborationService,
        document: &CollaborationDocumentId,
    ) -> StoreResult<Option<DurableCollaborationEnvelope>> {
        service.load(document)
    }
}

#[cfg(test)]
mod tests;

//! A storage port that keeps envelopes in memory, for Clerkenwell's own tests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{
    CollaborationAuditQuery, CollaborationAuditRecord, CollaborationDocumentId,
    CollaborationStoragePort, StoreError, StoreResult, StoredEnvelope,
};

#[derive(Debug, Default)]
struct MemoryState {
    next_version: u64,
    envelopes: BTreeMap<String, StoredEnvelope>,
    evidence: Vec<Vec<u8>>,
    audit: Vec<CollaborationAuditRecord>,
    reads: usize,
    writes: usize,
    withhold_stamps: bool,
    lose_swaps: bool,
    fail_swaps: bool,
}

/// Envelopes kept in memory under counter versions, whose stamp is the
/// version. Clones share what they keep.
#[derive(Debug, Clone, Default)]
pub struct MemoryStorage(Arc<Mutex<MemoryState>>);

impl MemoryStorage {
    /// A port that vouches for no stamp, so every reader reads the bytes.
    pub fn without_stamps() -> Self {
        let storage = Self::default();
        storage.state().withhold_stamps = true;
        storage
    }

    fn state(&self) -> MutexGuard<'_, MemoryState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many reads of an envelope's bytes the port has answered.
    pub fn reads(&self) -> usize {
        self.state().reads
    }

    /// How many times the port has been asked to change what it keeps:
    /// every swap, removal, preservation and audit record, kept or not.
    pub fn writes(&self) -> usize {
        self.state().writes
    }

    /// While `lose` holds, every swap reports that another writer won.
    pub fn lose_swaps(&self, lose: bool) {
        self.state().lose_swaps = lose;
    }

    /// While `fail` holds, every swap fails and keeps nothing.
    pub fn fail_swaps(&self, fail: bool) {
        self.state().fail_swaps = fail;
    }

    /// Every envelope the port keeps, by source.
    pub fn envelopes(&self) -> BTreeMap<String, StoredEnvelope> {
        self.state().envelopes.clone()
    }

    /// The bytes of every envelope preserved as evidence, oldest first.
    pub fn evidence(&self) -> Vec<Vec<u8>> {
        self.state().evidence.clone()
    }
}

impl CollaborationStoragePort for MemoryStorage {
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
        state.reads += 1;
        Ok(state.envelopes.get(source).cloned())
    }

    fn stamp(&self, source: &str) -> StoreResult<Option<String>> {
        let state = self.state();
        if state.withhold_stamps {
            return Ok(None);
        }
        Ok(state
            .envelopes
            .get(source)
            .map(|stored| stored.version.clone()))
    }

    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>> {
        let source = self.source(document);
        let mut state = self.state();
        state.writes += 1;
        if state.fail_swaps {
            return Err(StoreError::internal(format!(
                "Could not keep the envelope at {source}."
            )));
        }
        let current = state
            .envelopes
            .get(&source)
            .map(|stored| stored.version.as_str());
        if state.lose_swaps || current != expected {
            return Ok(None);
        }
        state.next_version += 1;
        let version = state.next_version.to_string();
        state.envelopes.insert(
            source,
            StoredEnvelope {
                version: version.clone(),
                bytes: bytes.to_vec(),
            },
        );
        Ok(Some(version))
    }

    fn compare_and_remove(
        &self,
        document: &CollaborationDocumentId,
        expected: &str,
    ) -> StoreResult<bool> {
        let source = self.source(document);
        let mut state = self.state();
        state.writes += 1;
        if state
            .envelopes
            .get(&source)
            .map(|stored| stored.version.as_str())
            != Some(expected)
        {
            return Ok(false);
        }
        state.envelopes.remove(&source);
        Ok(true)
    }

    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<String> {
        let mut state = self.state();
        state.writes += 1;
        state.evidence.push(bytes.to_vec());
        Ok(format!(
            "evidence/{}/{}/{}-{label}",
            document.entity,
            document.resource_id,
            state.evidence.len()
        ))
    }

    fn append_audit(&self, record: &CollaborationAuditRecord) -> StoreResult<()> {
        let mut state = self.state();
        state.writes += 1;
        state.audit.push(record.clone());
        Ok(())
    }

    fn audit(&self, query: &CollaborationAuditQuery) -> StoreResult<Vec<CollaborationAuditRecord>> {
        Ok(self
            .state()
            .audit
            .iter()
            .filter(|record| query.selects(record))
            .cloned()
            .collect())
    }

    fn discard_audit_before(&self, unix_ms: u128) -> StoreResult<usize> {
        let mut state = self.state();
        state.writes += 1;
        let kept = state.audit.len();
        state
            .audit
            .retain(|record| record.timestamp_unix_ms >= unix_ms);
        Ok(kept - state.audit.len())
    }
}

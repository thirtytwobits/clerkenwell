//! The storage port the example's stores keep their notes through.
//!
//! An application implements `CollaborationStoragePort` over its own storage.
//! The example keeps every envelope in memory, so a store lasts as long as
//! the process that holds it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use clerkenwell_store::{
    CollaborationDocumentId, CollaborationRecoveryAuditRecord, CollaborationStoragePort,
    StoreResult, StoredEnvelope,
};

#[derive(Debug, Default)]
struct Kept {
    next_version: u64,
    envelopes: BTreeMap<String, StoredEnvelope>,
    evidence: Vec<Vec<u8>>,
    audit: Vec<CollaborationRecoveryAuditRecord>,
}

/// Envelopes kept in memory, each under a version that counts the writes
/// before it. Clones share what they keep.
#[derive(Debug, Clone, Default)]
pub struct MemoryStorage(Arc<Mutex<Kept>>);

impl MemoryStorage {
    fn kept(&self) -> MutexGuard<'_, Kept> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn version(kept: &Kept, source: &str) -> Option<String> {
        kept.envelopes
            .get(source)
            .map(|stored| stored.version.clone())
    }
}

impl CollaborationStoragePort for MemoryStorage {
    fn source(&self, document: &CollaborationDocumentId) -> String {
        format!("{}/{}", document.entity, document.resource_id)
    }

    fn sources(&self, entity: &str) -> StoreResult<Vec<String>> {
        let prefix = format!("{entity}/");
        Ok(self
            .kept()
            .envelopes
            .keys()
            .filter(|source| source.starts_with(&prefix))
            .cloned()
            .collect())
    }

    fn read(&self, source: &str) -> StoreResult<Option<StoredEnvelope>> {
        Ok(self.kept().envelopes.get(source).cloned())
    }

    fn stamp(&self, source: &str) -> StoreResult<Option<String>> {
        Ok(Self::version(&self.kept(), source))
    }

    fn compare_and_swap(
        &self,
        document: &CollaborationDocumentId,
        expected: Option<&str>,
        bytes: &[u8],
    ) -> StoreResult<Option<String>> {
        let source = self.source(document);
        let mut kept = self.kept();
        if Self::version(&kept, &source).as_deref() != expected {
            return Ok(None);
        }
        kept.next_version += 1;
        let version = kept.next_version.to_string();
        kept.envelopes.insert(
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
        let mut kept = self.kept();
        if Self::version(&kept, &source).as_deref() != Some(expected) {
            return Ok(false);
        }
        kept.envelopes.remove(&source);
        Ok(true)
    }

    fn preserve(
        &self,
        document: &CollaborationDocumentId,
        label: &str,
        bytes: &[u8],
    ) -> StoreResult<String> {
        let mut kept = self.kept();
        kept.evidence.push(bytes.to_vec());
        Ok(format!(
            "evidence/{}/{}/{}-{label}",
            document.entity,
            document.resource_id,
            kept.evidence.len()
        ))
    }

    fn append_recovery_audit(&self, record: &CollaborationRecoveryAuditRecord) -> StoreResult<()> {
        self.kept().audit.push(record.clone());
        Ok(())
    }

    fn recovery_audit(&self) -> StoreResult<Vec<CollaborationRecoveryAuditRecord>> {
        Ok(self.kept().audit.clone())
    }
}

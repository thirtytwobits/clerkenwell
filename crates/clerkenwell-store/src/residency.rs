//! The documents a service holds in memory.
//!
//! A service keeps each document it reads as it was stored, so a later read
//! neither loads the envelope nor rebuilds the document again. It serves what
//! it holds only while the storage port vouches that the stored bytes are
//! unchanged; otherwise it reads the bytes and keeps what it holds only when
//! their version matches. A write that bypasses the service is therefore
//! seen by its next read.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use clerkenwell_doc::LoroAuthoringDocument;
use serde_json::Value;

use crate::{CollaborationRuntimeCounters, DurableCollaborationEnvelope, StoreResult};

/// A document as the service holds it: the envelope stored at `version`, and
/// the replica and materialised document built from it once a read needed
/// them.
#[derive(Debug)]
pub(crate) struct Resident {
    pub(crate) version: String,
    pub(crate) envelope: DurableCollaborationEnvelope,
    /// The storage port's stamp taken before these bytes were read, when the
    /// port could vouch for one.
    stamp: Mutex<Option<String>>,
    replica: Mutex<Option<Arc<LoroAuthoringDocument>>>,
    materialized: Mutex<Option<Arc<Value>>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Resident {
    pub(crate) fn new(
        version: String,
        stamp: Option<String>,
        envelope: DurableCollaborationEnvelope,
    ) -> Self {
        Self {
            version,
            envelope,
            stamp: Mutex::new(stamp),
            replica: Mutex::new(None),
            materialized: Mutex::new(None),
        }
    }

    /// Whether the storage port's `stamp` vouches for the bytes held.
    pub(crate) fn vouched_for_by(&self, stamp: Option<&str>) -> bool {
        stamp.is_some() && locked(&self.stamp).as_deref() == stamp
    }

    pub(crate) fn restamp(&self, stamp: Option<String>) {
        *locked(&self.stamp) = stamp;
    }

    /// The replica of the held document, built by `build` the first time it
    /// is needed.
    pub(crate) fn replica(
        &self,
        build: impl FnOnce(&DurableCollaborationEnvelope) -> StoreResult<LoroAuthoringDocument>,
    ) -> StoreResult<Arc<LoroAuthoringDocument>> {
        let mut replica = locked(&self.replica);
        if let Some(replica) = replica.as_ref() {
            return Ok(replica.clone());
        }
        let built = Arc::new(build(&self.envelope)?);
        *replica = Some(built.clone());
        Ok(built)
    }

    /// The held document materialised, by `build` the first time it is needed.
    pub(crate) fn materialized(
        &self,
        build: impl FnOnce() -> StoreResult<Value>,
    ) -> StoreResult<Arc<Value>> {
        let mut materialized = locked(&self.materialized);
        if let Some(value) = materialized.as_ref() {
            return Ok(value.clone());
        }
        let built = Arc::new(build()?);
        *materialized = Some(built.clone());
        Ok(built)
    }

    /// Keeps a replica and materialised document already built of the held
    /// state, as a commit has them.
    pub(crate) fn offer(&self, replica: Arc<LoroAuthoringDocument>, materialized: Value) {
        locked(&self.replica).get_or_insert(replica);
        locked(&self.materialized).get_or_insert_with(|| Arc::new(materialized));
    }

    /// Takes over what `earlier` built when it holds the same accepted state,
    /// as after a write that changed only an envelope's bookkeeping.
    fn inherit(&self, earlier: &Resident) {
        if earlier.envelope.checkpoint_sha256 != self.envelope.checkpoint_sha256
            || earlier.envelope.schema_version != self.envelope.schema_version
        {
            return;
        }
        if let Some(replica) = locked(&earlier.replica).clone() {
            locked(&self.replica).get_or_insert(replica);
        }
        if let Some(value) = locked(&earlier.materialized).clone() {
            locked(&self.materialized).get_or_insert(value);
        }
    }
}

/// Every document a service holds, by the storage source it was read from.
/// Clones of a service share one residency, and what it holds is released
/// when the last of them is dropped.
#[derive(Debug)]
pub(crate) struct Residency {
    held: Mutex<HashMap<String, Arc<Resident>>>,
    counters: Arc<CollaborationRuntimeCounters>,
}

impl Residency {
    pub(crate) fn new(counters: Arc<CollaborationRuntimeCounters>) -> Self {
        Self {
            held: Mutex::new(HashMap::new()),
            counters,
        }
    }

    pub(crate) fn get(&self, source: &str) -> Option<Arc<Resident>> {
        locked(&self.held).get(source).cloned()
    }

    /// Holds `resident` as `source`'s, taking over what the document it
    /// replaces had built of the same accepted state.
    pub(crate) fn hold(&self, source: &str, resident: Arc<Resident>) {
        let replaced = locked(&self.held).insert(source.to_string(), resident.clone());
        if let Some(earlier) = replaced {
            resident.inherit(&earlier);
            self.count(-1, &earlier);
        }
        self.count(1, &resident);
    }

    pub(crate) fn forget(&self, source: &str) {
        if let Some(earlier) = locked(&self.held).remove(source) {
            self.count(-1, &earlier);
        }
    }

    fn count(&self, sign: i8, resident: &Resident) {
        let bytes = resident.envelope.checkpoint_bytes as u64;
        let counters = &self.counters;
        if sign > 0 {
            counters.resident_documents.fetch_add(1, Ordering::Relaxed);
            counters
                .resident_checkpoint_bytes
                .fetch_add(bytes, Ordering::Relaxed);
        } else {
            counters.resident_documents.fetch_sub(1, Ordering::Relaxed);
            counters
                .resident_checkpoint_bytes
                .fetch_sub(bytes, Ordering::Relaxed);
        }
    }
}

impl Drop for Residency {
    fn drop(&mut self) {
        for (_, resident) in locked(&self.held).drain() {
            self.count(-1, &resident);
        }
    }
}

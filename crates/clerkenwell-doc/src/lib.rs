//! Plan-driven replicas of collaborative documents, built on Loro.
//!
//! A replica executes one generated collaboration plan: it seeds and reads a
//! Loro document in the plan's container layout, writes whole-document edits
//! as the operations between two versions, prepares text edits at a captured
//! frontier, and judges concurrent edits against each field's conflict policy.

mod policy;
mod substrate;

pub use policy::{conflicting_field_paths, creation_conflicting_field_paths};

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clerkenwell_schema::{ActorKind, GeneratedCollaborationEntitySpec};
use loro::{ExportMode, Frontiers, LoroDoc, VersionVector, ID};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationReplicaDebug {
    pub peer_id: String,
    pub oplog_version: String,
    pub state_frontiers: String,
}

#[derive(Debug, Error)]
pub enum CollaborationReplicaError {
    #[error("invalid update_base64: {0}")]
    InvalidBase64(String),
    #[error("could not process the collaboration update: {0}")]
    Update(String),
    #[error("could not encode the collaboration update: {0}")]
    Encode(String),
    #[error("collaborative document field `{path}` must be {expected}")]
    InvalidField {
        path: String,
        expected: &'static str,
    },
    #[error(
        "unsupported {entity} collaboration schema version {actual}; this adapter requires version {expected}"
    )]
    UnsupportedSchemaVersion {
        entity: &'static str,
        actual: u32,
        expected: u32,
    },
    #[error("the supplied collaboration frontier is not present in the document history")]
    UnknownFrontier,
    #[error("the collaboration update has causal dependencies that are not present")]
    MissingDependency,
    #[error("the collaboration update holds a history built apart from this document's")]
    UnrelatedHistory,
    #[error("the supplied text does not match the captured collaboration frontier")]
    TextCaptureMismatch,
}

fn update_error(error: loro::LoroError) -> CollaborationReplicaError {
    CollaborationReplicaError::Update(error.to_string())
}

fn encode_error(error: loro::LoroEncodeError) -> CollaborationReplicaError {
    CollaborationReplicaError::Encode(error.to_string())
}

fn base64_error(error: base64::DecodeError) -> CollaborationReplicaError {
    CollaborationReplicaError::InvalidBase64(error.to_string())
}

/// The revision a replica reads its own document at.
const REPLICA_REVISION: &str = "replica:materialized";

/// A retained replica executing one generated entity plan.
#[derive(Debug)]
pub struct CollaborationReplica {
    plan: &'static GeneratedCollaborationEntitySpec,
    doc: LoroDoc,
    /// The document the next edit is written against, or `None` when an
    /// imported update has changed the document since it was read.
    baseline: Option<Value>,
}

impl CollaborationReplica {
    /// A replica holding `document`, written as operations under `peer`.
    pub fn from_document(
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &Value,
        peer: u64,
    ) -> Result<Self, CollaborationReplicaError> {
        let document = substrate::without_null_optionals(plan, document);
        substrate::validate_document(plan, &document)?;
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).map_err(update_error)?;
        substrate::write_document_changes(plan, &doc, &Value::Null, &document)?;
        Ok(Self {
            plan,
            doc,
            baseline: Some(document),
        })
    }

    pub fn from_versioned_update_base64(
        plan: &'static GeneratedCollaborationEntitySpec,
        schema_version: u32,
        update_base64: &str,
    ) -> Result<Self, CollaborationReplicaError> {
        require_supported_schema_version(plan, schema_version)?;
        let doc = LoroDoc::new();
        let status = doc
            .import(&BASE64.decode(update_base64).map_err(base64_error)?)
            .map_err(update_error)?;
        if status.pending.is_some() {
            return Err(CollaborationReplicaError::MissingDependency);
        }
        let document = substrate::materialize_document(plan, &doc, REPLICA_REVISION)?;
        Ok(Self {
            plan,
            doc,
            baseline: Some(document),
        })
    }

    /// Writes `document` as the operations between it and the document the
    /// replica holds.
    pub fn replace_document(&mut self, document: &Value) -> Result<(), CollaborationReplicaError> {
        let document = substrate::without_null_optionals(self.plan, document);
        let baseline = match self.baseline.take() {
            Some(baseline) => baseline,
            None => substrate::materialize_document(self.plan, &self.doc, REPLICA_REVISION)?,
        };
        substrate::write_document_changes(self.plan, &self.doc, &baseline, &document)?;
        self.baseline = Some(document);
        Ok(())
    }

    /// Imports operations another writer made. A later edit is written
    /// against the document they leave.
    pub fn adopt_versioned_update_base64(
        &mut self,
        schema_version: u32,
        update_base64: &str,
    ) -> Result<(), CollaborationReplicaError> {
        require_supported_schema_version(self.plan, schema_version)?;
        self.require_related_history(update_base64)?;
        let status = self
            .doc
            .import(&BASE64.decode(update_base64).map_err(base64_error)?)
            .map_err(update_error)?;
        if status.pending.is_some() {
            return Err(CollaborationReplicaError::MissingDependency);
        }
        self.baseline = None;
        Ok(())
    }

    /// Refuses an update holding a root this replica lacks: a change that
    /// depends on nothing, so a history built apart from the replica's own.
    pub fn require_related_history(
        &self,
        update_base64: &str,
    ) -> Result<(), CollaborationReplicaError> {
        // A root depends on nothing, so it lands in an empty document even
        // when the rest of the update waits on operations held elsewhere.
        let scratch = LoroDoc::new();
        scratch
            .import(&BASE64.decode(update_base64).map_err(base64_error)?)
            .map_err(update_error)?;
        let held = self.doc.oplog_vv();
        let unrelated = scratch.oplog_vv().keys().any(|peer| {
            let first = ID::new(*peer, 0);
            scratch
                .get_change(first)
                .is_some_and(|change| change.deps.is_empty())
                && !held.includes_id(first)
        });
        if unrelated {
            return Err(CollaborationReplicaError::UnrelatedHistory);
        }
        Ok(())
    }

    /// Writes the replica's later edits as operations under `peer`.
    pub fn set_peer(&self, peer: u64) -> Result<(), CollaborationReplicaError> {
        self.doc.set_peer_id(peer).map_err(update_error)
    }

    /// Every peer `update_base64` holds operations of, in ascending order.
    pub fn peers_in_update(update_base64: &str) -> Result<Vec<u64>, CollaborationReplicaError> {
        let mut peers: Vec<u64> = update_end_version(update_base64)?.keys().copied().collect();
        peers.sort_unstable();
        Ok(peers)
    }

    /// The peers `update_base64` holds operations of that this replica
    /// lacks, in ascending order.
    pub fn peers_with_new_operations(
        &self,
        update_base64: &str,
    ) -> Result<Vec<u64>, CollaborationReplicaError> {
        let held = self.doc.oplog_vv();
        let mut peers: Vec<u64> = update_end_version(update_base64)?
            .iter()
            .filter(|(peer, end)| **end > held.get(peer).copied().unwrap_or(0))
            .map(|(peer, _)| *peer)
            .collect();
        peers.sort_unstable();
        Ok(peers)
    }

    pub fn export_update_base64(&self) -> Result<String, CollaborationReplicaError> {
        Ok(BASE64.encode(
            self.doc
                .export(ExportMode::all_updates())
                .map_err(encode_error)?,
        ))
    }

    pub fn export_incremental_update_base64(
        &self,
        accepted_frontier_base64: &str,
    ) -> Result<String, CollaborationReplicaError> {
        let frontiers = Frontiers::decode(
            &BASE64
                .decode(accepted_frontier_base64)
                .map_err(base64_error)?,
        )
        .map_err(update_error)?;
        let version = self
            .doc
            .frontiers_to_vv(&frontiers)
            .ok_or(CollaborationReplicaError::UnknownFrontier)?;
        Ok(BASE64.encode(
            self.doc
                .export(ExportMode::updates(&version))
                .map_err(encode_error)?,
        ))
    }

    /// The operations this document holds that a peer lacks, when the peer
    /// holds every operation up to `base_frontier_base64` (nothing, when
    /// `None`) and every operation in `update_base64`.
    pub fn missing_update_base64(
        &self,
        base_frontier_base64: Option<&str>,
        update_base64: &str,
    ) -> Result<String, CollaborationReplicaError> {
        let mut known = match base_frontier_base64 {
            Some(encoded) => self
                .doc
                .frontiers_to_vv(
                    &Frontiers::decode(&BASE64.decode(encoded).map_err(base64_error)?)
                        .map_err(update_error)?,
                )
                .ok_or(CollaborationReplicaError::UnknownFrontier)?,
            None => VersionVector::default(),
        };
        let sent = LoroDoc::decode_import_blob_meta(
            &BASE64.decode(update_base64).map_err(base64_error)?,
            false,
        )
        .map_err(update_error)?;
        known.merge(&sent.partial_end_vv);
        Ok(BASE64.encode(
            self.doc
                .export(ExportMode::updates(&known))
                .map_err(encode_error)?,
        ))
    }

    pub fn accepted_frontier_base64(&self) -> String {
        BASE64.encode(self.doc.oplog_frontiers().encode())
    }

    pub fn require_frontier_base64(
        &self,
        accepted_frontier_base64: &str,
    ) -> Result<(), CollaborationReplicaError> {
        let frontiers = Frontiers::decode(
            &BASE64
                .decode(accepted_frontier_base64)
                .map_err(base64_error)?,
        )
        .map_err(update_error)?;
        self.doc
            .frontiers_to_vv(&frontiers)
            .ok_or(CollaborationReplicaError::UnknownFrontier)?;
        Ok(())
    }

    /// Whether a capture includes every operation required by a prior domain action.
    pub fn frontier_includes(
        &self,
        capture_base64: &str,
        required_base64: &str,
    ) -> Result<bool, CollaborationReplicaError> {
        let version = |encoded: &str| {
            let frontier = Frontiers::decode(&BASE64.decode(encoded).map_err(base64_error)?)
                .map_err(update_error)?;
            self.doc
                .frontiers_to_vv(&frontier)
                .ok_or(CollaborationReplicaError::UnknownFrontier)
        };
        let capture = version(capture_base64)?;
        let required = version(required_base64)?;
        Ok(matches!(
            capture.partial_cmp(&required),
            Some(std::cmp::Ordering::Equal | std::cmp::Ordering::Greater)
        ))
    }

    /// Read a declared text field at a captured frontier without changing the live replica.
    pub fn text_at_frontier(
        &self,
        field_path: &str,
        identities: &HashMap<String, String>,
        frontier_base64: &str,
    ) -> Result<String, CollaborationReplicaError> {
        let branch = self.fork_at_frontier(frontier_base64, None)?;
        Ok(substrate::declared_text(self.plan, &branch, field_path, identities)?.to_string())
    }

    /// Prepare operations replacing exactly the captured text. An empty replacement
    /// consumes those characters; concurrent insertions survive import of the result.
    /// Preparation is read-only. The owner must durably accept and deduplicate its
    /// command before publishing these operations, and reject overlapping consumption.
    /// Retain the returned payload for retries; preparing again creates new operation IDs.
    /// The operations are written under `peer`, which no other replica writes under.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_text_replacement_at_frontier(
        &self,
        field_path: &str,
        identities: &HashMap<String, String>,
        frontier_base64: &str,
        expected_text: &str,
        replacement: &str,
        peer: u64,
    ) -> Result<String, CollaborationReplicaError> {
        let branch = self.fork_at_frontier(frontier_base64, Some(peer))?;
        let text = substrate::declared_text(self.plan, &branch, field_path, identities)?;
        // Membership must also hold at the live head: retained history is not
        // permission to write an orphaned container after its record was deleted.
        substrate::declared_text(self.plan, &self.doc, field_path, identities)?;
        if text.to_string() != expected_text {
            return Err(CollaborationReplicaError::TextCaptureMismatch);
        }
        let version = branch.oplog_vv();
        substrate::replace_all_text(&text, replacement).map_err(update_error)?;
        branch.commit();
        Ok(BASE64.encode(
            branch
                .export(ExportMode::updates(&version))
                .map_err(encode_error)?,
        ))
    }

    /// Prepare an insertion before captured text without replacing any captured
    /// character identities. Concurrent operations remain mergeable on import.
    /// The operations are written under `peer`, which no other replica writes under.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_text_prefix_at_frontier(
        &self,
        field_path: &str,
        identities: &HashMap<String, String>,
        frontier_base64: &str,
        expected_text: &str,
        prefix: &str,
        peer: u64,
    ) -> Result<String, CollaborationReplicaError> {
        let branch = self.fork_at_frontier(frontier_base64, Some(peer))?;
        let text = substrate::declared_text(self.plan, &branch, field_path, identities)?;
        substrate::declared_text(self.plan, &self.doc, field_path, identities)?;
        if text.to_string() != expected_text {
            return Err(CollaborationReplicaError::TextCaptureMismatch);
        }
        let version = branch.oplog_vv();
        text.insert_utf8(0, prefix).map_err(update_error)?;
        branch.commit();
        Ok(BASE64.encode(
            branch
                .export(ExportMode::updates(&version))
                .map_err(encode_error)?,
        ))
    }

    /// A branch of the document at `frontier_base64`, writing under `peer`
    /// when it is given one.
    fn fork_at_frontier(
        &self,
        frontier_base64: &str,
        peer: Option<u64>,
    ) -> Result<LoroDoc, CollaborationReplicaError> {
        let frontier = Frontiers::decode(&BASE64.decode(frontier_base64).map_err(base64_error)?)
            .map_err(update_error)?;
        let branch = self
            .doc
            .fork_at(&frontier)
            .map_err(|_| CollaborationReplicaError::UnknownFrontier)?;
        if let Some(peer) = peer {
            branch.set_peer_id(peer).map_err(update_error)?;
        }
        Ok(branch)
    }

    pub fn materialized_documents_for_incremental_update(
        &self,
        accepted_frontier_base64: &str,
        schema_version: u32,
        update_base64: &str,
        revision: &str,
    ) -> Result<(Value, Value), CollaborationReplicaError> {
        require_supported_schema_version(self.plan, schema_version)?;
        let frontiers = Frontiers::decode(
            &BASE64
                .decode(accepted_frontier_base64)
                .map_err(base64_error)?,
        )
        .map_err(update_error)?;
        let base = self
            .doc
            .fork_at(&frontiers)
            .map_err(|_| CollaborationReplicaError::UnknownFrontier)?;
        let client = base.fork();
        let status = client
            .import(&BASE64.decode(update_base64).map_err(base64_error)?)
            .map_err(update_error)?;
        if status.pending.is_some() {
            return Err(CollaborationReplicaError::MissingDependency);
        }
        Ok((
            substrate::materialize_document(self.plan, &base, revision)?,
            substrate::materialize_document(self.plan, &client, revision)?,
        ))
    }

    /// The declared fields an incremental update `kind` made changes against
    /// their conflict policy for `kind`, judged against the concurrent edits
    /// this replica already holds beyond the update's base frontier.
    pub fn policy_conflicts_for_incremental_update(
        &self,
        base_frontier_base64: &str,
        schema_version: u32,
        update_base64: &str,
        kind: ActorKind,
    ) -> Result<Vec<String>, CollaborationReplicaError> {
        let (base, client) = self.materialized_documents_for_incremental_update(
            base_frontier_base64,
            schema_version,
            update_base64,
            "replica:policy",
        )?;
        let current = self.materialized_document("replica:policy")?;
        Ok(conflicting_field_paths(
            self.plan, &base, &client, &current, kind,
        ))
    }

    /// The declared fields this replica's document, created by `kind`, sets
    /// against the policy that replaces their own for `kind`.
    pub fn policy_conflicts_for_creation(
        &self,
        kind: ActorKind,
    ) -> Result<Vec<String>, CollaborationReplicaError> {
        let created = self.materialized_document("replica:policy")?;
        Ok(creation_conflicting_field_paths(self.plan, &created, kind))
    }

    pub fn debug(&self) -> CollaborationReplicaDebug {
        CollaborationReplicaDebug {
            peer_id: self.doc.peer_id().to_string(),
            oplog_version: format!("{:?}", self.doc.oplog_vv()),
            state_frontiers: format!("{:?}", self.doc.state_frontiers()),
        }
    }

    pub fn materialized_document(
        &self,
        revision: &str,
    ) -> Result<Value, CollaborationReplicaError> {
        substrate::materialize_document(self.plan, &self.doc, revision)
    }
}

/// For each peer `update_base64` holds operations of, the counter after its
/// last one.
fn update_end_version(update_base64: &str) -> Result<VersionVector, CollaborationReplicaError> {
    Ok(LoroDoc::decode_import_blob_meta(
        &BASE64.decode(update_base64).map_err(base64_error)?,
        false,
    )
    .map_err(update_error)?
    .partial_end_vv)
}

fn require_supported_schema_version(
    plan: &'static GeneratedCollaborationEntitySpec,
    schema_version: u32,
) -> Result<(), CollaborationReplicaError> {
    if schema_version != plan.schema_version {
        return Err(CollaborationReplicaError::UnsupportedSchemaVersion {
            entity: plan.name,
            actual: schema_version,
            expected: plan.schema_version,
        });
    }
    Ok(())
}

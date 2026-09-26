//! Plan-driven Loro replicas of collaborative documents.
//!
//! A replica executes one generated collaboration plan: it seeds and reads a
//! Loro document in the plan's container layout, writes whole-document edits
//! as the operations between two versions, prepares text edits at a captured
//! frontier, and judges concurrent edits against each field's conflict policy.

mod policy;
mod substrate;

pub use loro;
pub use policy::conflicting_field_paths;

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clerkenwell_schema::GeneratedCollaborationEntitySpec;
use loro::{ExportMode, Frontiers, LoroDoc, LoroValue, VersionVector};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationLoroDebug {
    pub peer_id: String,
    pub oplog_version: String,
    pub state_frontiers: String,
}

#[derive(Debug, Error)]
pub enum CollaborationLoroError {
    #[error("invalid Loro update_base64: {0}")]
    InvalidBase64(#[from] base64::DecodeError),
    #[error("could not process Loro collaboration update: {0}")]
    Loro(#[from] loro::LoroError),
    #[error("could not encode Loro collaboration update: {0}")]
    LoroEncode(#[from] loro::LoroEncodeError),
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
    #[error("the supplied text does not match the captured collaboration frontier")]
    TextCaptureMismatch,
}

/// A retained Loro document executing one generated entity plan.
#[derive(Debug)]
pub struct LoroAuthoringDocument {
    plan: &'static GeneratedCollaborationEntitySpec,
    doc: LoroDoc,
    document: Value,
}

impl LoroAuthoringDocument {
    pub fn from_document(
        plan: &'static GeneratedCollaborationEntitySpec,
        document: &Value,
    ) -> Result<Self, CollaborationLoroError> {
        let document = substrate::without_null_optionals(plan, document);
        substrate::validate_document(plan, &document)?;
        let doc = LoroDoc::new();
        substrate::write_document_changes(plan, &doc, &Value::Null, &document)?;
        Ok(Self {
            plan,
            doc,
            document,
        })
    }

    pub fn from_versioned_update_base64(
        plan: &'static GeneratedCollaborationEntitySpec,
        schema_version: u32,
        update_base64: &str,
    ) -> Result<Self, CollaborationLoroError> {
        require_supported_schema_version(plan, schema_version)?;
        let doc = LoroDoc::new();
        let status = doc.import(&BASE64.decode(update_base64)?)?;
        if status.pending.is_some() {
            return Err(CollaborationLoroError::MissingDependency);
        }
        let document = substrate::materialize_document(plan, &doc, "loro:materialized")?;
        Ok(Self {
            plan,
            doc,
            document,
        })
    }

    pub fn replace_document(&mut self, document: &Value) -> Result<(), CollaborationLoroError> {
        let document = substrate::without_null_optionals(self.plan, document);
        substrate::write_document_changes(self.plan, &self.doc, &self.document, &document)?;
        self.document = document;
        Ok(())
    }

    pub fn adopt_versioned_update_base64(
        &mut self,
        schema_version: u32,
        update_base64: &str,
    ) -> Result<(), CollaborationLoroError> {
        require_supported_schema_version(self.plan, schema_version)?;
        let status = self.doc.import(&BASE64.decode(update_base64)?)?;
        if status.pending.is_some() {
            return Err(CollaborationLoroError::MissingDependency);
        }
        self.document = substrate::materialize_document(self.plan, &self.doc, "loro:materialized")?;
        Ok(())
    }

    pub fn import_versioned_update_base64(
        &self,
        schema_version: u32,
        update_base64: &str,
    ) -> Result<(), CollaborationLoroError> {
        require_supported_schema_version(self.plan, schema_version)?;
        let status = self.doc.import(&BASE64.decode(update_base64)?)?;
        if status.pending.is_some() {
            return Err(CollaborationLoroError::MissingDependency);
        }
        Ok(())
    }

    pub fn export_update_base64(&self) -> Result<String, CollaborationLoroError> {
        Ok(BASE64.encode(self.doc.export(ExportMode::all_updates())?))
    }

    pub fn export_incremental_update_base64(
        &self,
        accepted_frontier_base64: &str,
    ) -> Result<String, CollaborationLoroError> {
        let frontiers = Frontiers::decode(&BASE64.decode(accepted_frontier_base64)?)?;
        let version = self
            .doc
            .frontiers_to_vv(&frontiers)
            .ok_or(CollaborationLoroError::UnknownFrontier)?;
        Ok(BASE64.encode(self.doc.export(ExportMode::updates(&version))?))
    }

    /// The operations this document holds that a peer lacks, when the peer
    /// holds every operation up to `base_frontier_base64` (nothing, when
    /// `None`) and every operation in `update_base64`.
    pub fn missing_update_base64(
        &self,
        base_frontier_base64: Option<&str>,
        update_base64: &str,
    ) -> Result<String, CollaborationLoroError> {
        let mut known = match base_frontier_base64 {
            Some(encoded) => self
                .doc
                .frontiers_to_vv(&Frontiers::decode(&BASE64.decode(encoded)?)?)
                .ok_or(CollaborationLoroError::UnknownFrontier)?,
            None => VersionVector::default(),
        };
        let sent = LoroDoc::decode_import_blob_meta(&BASE64.decode(update_base64)?, false)?;
        known.merge(&sent.partial_end_vv);
        Ok(BASE64.encode(self.doc.export(ExportMode::updates(&known))?))
    }

    pub fn accepted_frontier_base64(&self) -> String {
        BASE64.encode(self.doc.oplog_frontiers().encode())
    }

    pub fn require_frontier_base64(
        &self,
        accepted_frontier_base64: &str,
    ) -> Result<(), CollaborationLoroError> {
        let frontiers = Frontiers::decode(&BASE64.decode(accepted_frontier_base64)?)?;
        self.doc
            .frontiers_to_vv(&frontiers)
            .ok_or(CollaborationLoroError::UnknownFrontier)?;
        Ok(())
    }

    /// Whether a capture includes every operation required by a prior domain action.
    pub fn frontier_includes(
        &self,
        capture_base64: &str,
        required_base64: &str,
    ) -> Result<bool, CollaborationLoroError> {
        let version = |encoded: &str| {
            let frontier = Frontiers::decode(&BASE64.decode(encoded)?)?;
            self.doc
                .frontiers_to_vv(&frontier)
                .ok_or(CollaborationLoroError::UnknownFrontier)
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
    ) -> Result<String, CollaborationLoroError> {
        let branch = self.fork_at_frontier(frontier_base64)?;
        Ok(substrate::declared_text(self.plan, &branch, field_path, identities)?.to_string())
    }

    /// Prepare operations replacing exactly the captured text. An empty replacement
    /// consumes those characters; concurrent insertions survive import of the result.
    /// Preparation is read-only. The owner must durably accept and deduplicate its
    /// command before publishing these operations, and reject overlapping consumption.
    /// Retain the returned payload for retries; preparing again creates new operation IDs.
    pub fn prepare_text_replacement_at_frontier(
        &self,
        field_path: &str,
        identities: &HashMap<String, String>,
        frontier_base64: &str,
        expected_text: &str,
        replacement: &str,
    ) -> Result<String, CollaborationLoroError> {
        let branch = self.fork_at_frontier(frontier_base64)?;
        let text = substrate::declared_text(self.plan, &branch, field_path, identities)?;
        // Membership must also hold at the live head: retained history is not
        // permission to write an orphaned container after its record was deleted.
        substrate::declared_text(self.plan, &self.doc, field_path, identities)?;
        if text.to_string() != expected_text {
            return Err(CollaborationLoroError::TextCaptureMismatch);
        }
        let version = branch.oplog_vv();
        substrate::replace_all_text(&text, replacement)?;
        branch.commit();
        Ok(BASE64.encode(branch.export(ExportMode::updates(&version))?))
    }

    /// Prepare an insertion before captured text without replacing any captured
    /// character identities. Concurrent operations remain mergeable on import.
    pub fn prepare_text_prefix_at_frontier(
        &self,
        field_path: &str,
        identities: &HashMap<String, String>,
        frontier_base64: &str,
        expected_text: &str,
        prefix: &str,
    ) -> Result<String, CollaborationLoroError> {
        let branch = self.fork_at_frontier(frontier_base64)?;
        let text = substrate::declared_text(self.plan, &branch, field_path, identities)?;
        substrate::declared_text(self.plan, &self.doc, field_path, identities)?;
        if text.to_string() != expected_text {
            return Err(CollaborationLoroError::TextCaptureMismatch);
        }
        let version = branch.oplog_vv();
        text.insert_utf8(0, prefix)?;
        branch.commit();
        Ok(BASE64.encode(branch.export(ExportMode::updates(&version))?))
    }

    fn fork_at_frontier(&self, frontier_base64: &str) -> Result<LoroDoc, CollaborationLoroError> {
        let frontier = Frontiers::decode(&BASE64.decode(frontier_base64)?)?;
        self.doc
            .fork_at(&frontier)
            .map_err(|_| CollaborationLoroError::UnknownFrontier)
    }

    pub fn materialized_documents_for_incremental_update(
        &self,
        accepted_frontier_base64: &str,
        schema_version: u32,
        update_base64: &str,
        revision: &str,
    ) -> Result<(Value, Value), CollaborationLoroError> {
        require_supported_schema_version(self.plan, schema_version)?;
        let frontiers = Frontiers::decode(&BASE64.decode(accepted_frontier_base64)?)?;
        let base = self
            .doc
            .fork_at(&frontiers)
            .map_err(|_| CollaborationLoroError::UnknownFrontier)?;
        let client = base.fork();
        let status = client.import(&BASE64.decode(update_base64)?)?;
        if status.pending.is_some() {
            return Err(CollaborationLoroError::MissingDependency);
        }
        Ok((
            substrate::materialize_document(self.plan, &base, revision)?,
            substrate::materialize_document(self.plan, &client, revision)?,
        ))
    }

    /// The declared fields an incremental update changes against their
    /// conflict policy, judged against the concurrent edits this replica
    /// already holds beyond the update's base frontier.
    pub fn policy_conflicts_for_incremental_update(
        &self,
        base_frontier_base64: &str,
        schema_version: u32,
        update_base64: &str,
    ) -> Result<Vec<String>, CollaborationLoroError> {
        let (base, client) = self.materialized_documents_for_incremental_update(
            base_frontier_base64,
            schema_version,
            update_base64,
            "loro:policy",
        )?;
        let current = self.materialized_document("loro:policy")?;
        Ok(conflicting_field_paths(self.plan, &base, &client, &current))
    }

    pub fn debug(&self) -> CollaborationLoroDebug {
        CollaborationLoroDebug {
            peer_id: self.doc.peer_id().to_string(),
            oplog_version: format!("{:?}", self.doc.oplog_vv()),
            state_frontiers: format!("{:?}", self.doc.state_frontiers()),
        }
    }

    pub fn materialized_document(&self, revision: &str) -> Result<Value, CollaborationLoroError> {
        substrate::materialize_document(self.plan, &self.doc, revision)
    }
}

pub fn write_document_json_to_loro_doc(
    plan: &'static GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    previous: &Value,
    next: &Value,
) -> Result<(), CollaborationLoroError> {
    substrate::write_document_changes(plan, doc, previous, next)
}

pub fn materialize_document_json_from_loro_doc(
    plan: &'static GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    revision: &str,
) -> Result<Value, CollaborationLoroError> {
    substrate::materialize_document(plan, doc, revision)
}

/// Materialises `doc` for a migration from a layout that stored no MIME type
/// for property text: each such field takes the `text/` MIME type at the same
/// path in `mime_source`, or `default_mime`.
pub fn materialize_document_resolving_text_mime(
    plan: &'static GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    revision: &str,
    mime_source: &Value,
    default_mime: &str,
) -> Result<Value, CollaborationLoroError> {
    substrate::materialize_document_resolving_text_mime(
        plan,
        doc,
        revision,
        mime_source,
        default_mime,
    )
}

/// The Loro value a JSON value is stored as.
pub fn json_to_loro(value: &Value) -> Result<LoroValue, CollaborationLoroError> {
    substrate::json_to_loro(value)
}

fn require_supported_schema_version(
    plan: &'static GeneratedCollaborationEntitySpec,
    schema_version: u32,
) -> Result<(), CollaborationLoroError> {
    if schema_version != plan.schema_version {
        return Err(CollaborationLoroError::UnsupportedSchemaVersion {
            entity: plan.name,
            actual: schema_version,
            expected: plan.schema_version,
        });
    }
    Ok(())
}

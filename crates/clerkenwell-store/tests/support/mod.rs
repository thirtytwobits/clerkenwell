//! A small plan the integration tests share.

// Each test binary uses a different part of this module.
#![allow(dead_code)]

use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec,
};
use clerkenwell_store::StoreResult;
use serde_json::Value;

const fn field(
    path: &'static str,
    storage_kind: GeneratedCollaborationStorageKind,
    container: Option<&'static str>,
    key: Option<&'static str>,
    conflict: GeneratedCollaborationConflict,
) -> GeneratedCollaborationFieldSpec {
    GeneratedCollaborationFieldSpec {
        path,
        storage_kind,
        container,
        container_template: None,
        key,
        identity_path: None,
        identity_variable: None,
        order_container: None,
        item_container_template: None,
        metadata_container: None,
        metadata_container_template: None,
        metadata_key: None,
        codec: GeneratedCollaborationValueCodec::String,
        value_schema: None,
        required: true,
        conflict,
    }
}

static NOTE_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    field(
        "note_id",
        GeneratedCollaborationStorageKind::Scalar,
        Some("note"),
        Some("note_id"),
        GeneratedCollaborationConflict::Immutable,
    ),
    field(
        "body",
        GeneratedCollaborationStorageKind::Text,
        Some("body"),
        None,
        GeneratedCollaborationConflict::Merge,
    ),
    field(
        "etag",
        GeneratedCollaborationStorageKind::DerivedRevision,
        None,
        None,
        GeneratedCollaborationConflict::Immutable,
    ),
];
pub static NOTE_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Note",
    id_field: "note_id",
    substrate: "loro",
    schema_version: 1,
    migration_ids: &[],
    authoring_projection: "notes.authoringState",
    import_mutation: "note.importLoroUpdate",
    root_container: "note",
    fields: NOTE_FIELDS,
};
/// A validator that accepts every document.
pub fn accept(_: &Value) -> StoreResult<()> {
    Ok(())
}

pub static PLANS: &[GeneratedCollaborationEntitySpec] = &[NOTE_PLAN];

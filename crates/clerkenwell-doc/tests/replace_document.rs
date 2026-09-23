//! A whole-document edit is validated before anything is written.

use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec,
};
use serde_json::{json, Value};

const fn field(
    path: &'static str,
    storage_kind: GeneratedCollaborationStorageKind,
    container: &'static str,
    key: Option<&'static str>,
    codec: GeneratedCollaborationValueCodec,
    required: bool,
) -> GeneratedCollaborationFieldSpec {
    GeneratedCollaborationFieldSpec {
        path,
        storage_kind,
        container: Some(container),
        container_template: None,
        key,
        identity_path: None,
        identity_variable: None,
        order_container: None,
        item_container_template: None,
        metadata_container: None,
        metadata_container_template: None,
        metadata_key: None,
        codec,
        value_schema: None,
        required,
        conflict: GeneratedCollaborationConflict::LastWriterWins,
    }
}

use GeneratedCollaborationStorageKind::{Scalar, Text};
use GeneratedCollaborationValueCodec::{OptionalNumber, OptionalString, String as Str};

static NOTE_FIELDS: &[GeneratedCollaborationFieldSpec] = &[
    field("note_id", Scalar, "note", Some("note_id"), Str, true),
    field("title", Scalar, "note", Some("title"), Str, true),
    field(
        "rating",
        Scalar,
        "note",
        Some("rating"),
        OptionalNumber,
        false,
    ),
    field(
        "subtitle",
        Scalar,
        "note",
        Some("subtitle"),
        OptionalString,
        false,
    ),
    field("body", Text, "body", None, Str, true),
];
static NOTE_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
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

fn note() -> Value {
    json!({
        "note_id": "note-1",
        "title": "Shopping",
        "body": "Bread and milk.",
    })
}

fn replica() -> LoroAuthoringDocument {
    LoroAuthoringDocument::from_document(&NOTE_PLAN, &note()).expect("seed replica")
}

/// Everything an observer can see of a replica: its operations and its document.
fn observed(replica: &LoroAuthoringDocument) -> (String, Value) {
    (
        replica.accepted_frontier_base64(),
        replica.materialized_document("rev").expect("materialise"),
    )
}

fn with(changes: Value) -> Value {
    let mut document = note();
    for (key, value) in changes.as_object().expect("changes") {
        document[key] = value.clone();
    }
    document
}

#[test]
fn a_document_missing_a_required_field_is_refused_and_changes_nothing() {
    let mut replica = replica();
    let before = observed(&replica);
    let mut missing_title = with(json!({ "body": "An edit that must not land." }));
    missing_title
        .as_object_mut()
        .expect("document")
        .remove("title");

    assert!(replica.replace_document(&missing_title).is_err());
    assert_eq!(observed(&replica), before);
}

#[test]
fn an_optional_value_its_codec_refuses_is_refused_before_anything_is_written() {
    let mut replica = replica();
    let before = observed(&replica);
    for refused in [json!("four"), json!(true), json!({ "stars": 4 })] {
        let edit = with(json!({ "body": "An edit that must not land.", "rating": refused }));
        assert!(
            replica.replace_document(&edit).is_err(),
            "rating {refused} must be refused"
        );
        assert_eq!(
            observed(&replica),
            before,
            "rating {refused} wrote something"
        );
    }
}

#[test]
fn an_optional_value_its_codec_accepts_is_kept() {
    let mut replica = replica();
    let rating = 4.5;
    let subtitle = "Weekly";
    let edit = with(json!({ "rating": rating, "subtitle": subtitle }));
    replica
        .replace_document(&edit)
        .expect("valid optional values");
    let document = replica.materialized_document("rev").expect("materialise");
    assert_eq!(document["rating"], json!(rating));
    assert_eq!(document["subtitle"], json!(subtitle));
}

#[test]
fn an_optional_field_written_as_null_or_left_out_is_removed() {
    let set = with(json!({ "rating": 4, "subtitle": "Weekly" }));
    let written_null = with(json!({ "rating": null, "subtitle": null }));
    let left_out = note();
    for removal in [written_null, left_out] {
        let mut replica = replica();
        replica.replace_document(&set).expect("set optional values");
        replica
            .replace_document(&removal)
            .expect("remove optional values");
        let document = replica.materialized_document("rev").expect("materialise");
        assert!(document.get("rating").is_none(), "{document}");
        assert!(document.get("subtitle").is_none(), "{document}");
    }
}

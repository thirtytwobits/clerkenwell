//! A definition outside what the generator can represent exactly is refused,
//! and the refusal names the definition path at fault.

mod common;

use clerkenwell_codegen::testing::{self, Json};
use clerkenwell_codegen::{build_outputs, Error};
use common::{
    json, merge, notebook_config, notebook_document, push, refusal, remove, set, validate,
    with_defs,
};

fn assert_mentions(message: &str, fragments: &[&str]) {
    for fragment in fragments {
        assert!(
            message.contains(fragment),
            "expected {fragment:?} in the refusal: {message}"
        );
    }
}

#[test]
fn invalid_projection_refs_fail_with_a_useful_message() {
    let mut document = notebook_document();
    set(
        &mut document,
        "/projections/notes.list/snapshot",
        json(r##"{ "$ref": "#/$defs/Missing" }"##),
    );
    assert_mentions(
        &refusal(document),
        &["projections.notes.list.snapshot references unknown definition \"Missing\""],
    );
}

#[test]
fn unsupported_schema_constructs_fail_with_a_useful_message() {
    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/NoteSummary/patternProperties",
        json("{}"),
    );
    assert_mentions(
        &refusal(document),
        &["$defs.NoteSummary uses unsupported schema keyword \"patternProperties\""],
    );
}

/// `const` and `minLength` constrain a value without changing its type, so no
/// renderer reads them and none would catch one attached to a type it cannot
/// apply to. Validation is the only place that can.
#[test]
fn validation_only_keywords_are_checked_against_the_type_they_constrain() {
    let cases = [
        (
            "priority",
            r#"{ "minLength": 1 }"#,
            "minLength is supported only on string schemas",
        ),
        (
            "title",
            r#"{ "minLength": -1 }"#,
            "minLength must be a non-negative integer",
        ),
        (
            "title",
            r#"{ "const": 1 }"#,
            "const must be a string to match its declared type",
        ),
        (
            "priority",
            r#"{ "const": 1.5 }"#,
            "const must be an integer to match its declared type",
        ),
        (
            "tags",
            r#"{ "const": [] }"#,
            "const is supported only on scalar schemas",
        ),
        (
            "status",
            r#"{ "const": "draft" }"#,
            "may not declare both const and enum",
        ),
    ];
    for (property, patch, expected) in cases {
        let mut document = notebook_document();
        let pointer = format!("/$defs/NoteDocument/properties/{property}");
        merge(&mut document, &pointer, json(patch));
        assert_mentions(
            &refusal(document),
            &[
                &format!("$defs.NoteDocument.properties.{property}"),
                expected,
            ],
        );
    }
}

/// Removes every validation-only keyword from the schema nodes under `schema`.
fn strip_validation_keywords(schema: &mut Json) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    object.remove("const");
    object.remove("minLength");
    for key in ["properties", "oneOf"] {
        if let Some(children) = object.get_mut(key).and_then(Json::as_object_mut) {
            let names: Vec<String> = children.keys().map(str::to_owned).collect();
            for name in names {
                strip_validation_keywords(children.get_mut(&name).expect("listed above"));
            }
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(child) = object.get_mut(key) {
            strip_validation_keywords(child);
        }
    }
}

/// The type renderers switch on `type` and ignore everything else, which is
/// what lets a constraint ride through without widening a generated type. The
/// constraints reach the Rust output only as `schemars` attributes, as an
/// open object's `OTHER_MEMBER_CONSTRAINTS` and as a record's
/// `VALUE_CONSTRAINTS`.
#[test]
fn validation_only_keywords_leave_the_generated_types_alone() {
    let config = notebook_config();
    let constrained = notebook_document();
    let mut unconstrained = constrained.clone();
    let names: Vec<String> = unconstrained
        .pointer("/$defs")
        .and_then(Json::as_object)
        .expect("the definition has $defs")
        .keys()
        .map(str::to_owned)
        .collect();
    for name in names {
        let pointer = format!("/$defs/{name}");
        strip_validation_keywords(unconstrained.pointer_mut(&pointer).expect("listed above"));
    }
    assert_ne!(
        constrained, unconstrained,
        "the example must carry constraints"
    );

    let render = |document: Json| {
        let definition = testing::definition(document).expect("valid");
        build_outputs(&definition, &config).expect("renders")
    };
    let with = render(constrained);
    let without = render(unconstrained);

    assert_eq!(with.typescript_model, without.typescript_model);
    assert_ne!(with.rust_model, without.rust_model);
    let types_only = |rust: &str| -> Vec<String> {
        let mut in_constraints = false;
        rust.lines()
            .filter(|line| {
                let line = line.trim_start();
                if line.starts_with("const OTHER_MEMBER_CONSTRAINTS")
                    || line.starts_with("const VALUE_CONSTRAINTS")
                    || (line.starts_with("pub const ") && line.contains("_SCHEMA: &str ="))
                {
                    in_constraints = true;
                }
                let constraint = in_constraints;
                if in_constraints && line.ends_with(';') {
                    in_constraints = false;
                }
                !constraint && !line.starts_with("#[schemars(")
            })
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        types_only(&with.rust_model),
        types_only(&without.rust_model)
    );
}

#[test]
fn invalid_collaboration_metadata_fails_with_a_useful_message() {
    let mut document = notebook_document();
    let note = document
        .pointer("/collaboration/entities/Note")
        .expect("the example collaborates on notes")
        .clone();
    set(&mut document, "/collaboration/entities/Missing", note);
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Missing references unknown entity \"Missing\""],
    );
}

#[test]
fn mutation_effects_and_materialisation_strategy_reject_definition_drift() {
    let mut unknown_touch = notebook_document();
    set(
        &mut unknown_touch,
        "/mutations/note.rename/touches",
        json(r#"["Missing"]"#),
    );
    assert_mentions(
        &refusal(unknown_touch),
        &["mutations.note.rename.touches references unknown entity \"Missing\""],
    );

    let mut wrong_strategy = notebook_document();
    set(
        &mut wrong_strategy,
        "/projections/notes.list/materialization",
        json(r#"{ "strategy": "replace", "snapshotMode": "patch" }"#),
    );
    assert_mentions(
        &refusal(wrong_strategy),
        &["notes.list.materialization.strategy \"replace\" expects patch kinds replace"],
    );
}

#[test]
fn collaboration_metadata_covers_the_complete_entity_schema() {
    let mut document = notebook_document();
    remove(
        &mut document,
        "/collaboration/entities/Board/fields/columns.*.title",
    );
    assert_mentions(
        &refusal(document),
        &["does not cover entity schema paths: columns.*.title"],
    );
}

/// An uncovered path is named as the shallowest path under which no field
/// covers anything: an object no field stores is named once.
#[test]
fn an_uncovered_path_is_named_at_the_shallowest_object_left_uncovered() {
    let refusal_without = |fields: &[&str]| {
        let mut document = notebook_document();
        for field in fields {
            remove(
                &mut document,
                &format!("/collaboration/entities/Note/fields/{field}"),
            );
        }
        match testing::definition(document) {
            Err(Error::Definition(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    };

    let body = refusal_without(&["body"]);
    assert!(
        body.ends_with("does not cover entity schema paths: body."),
        "{body}"
    );
    let both = refusal_without(&["meta.owner", "meta.reviewer"]);
    assert!(
        both.ends_with("does not cover entity schema paths: meta."),
        "{both}"
    );
    let one = refusal_without(&["meta.reviewer"]);
    assert!(
        one.ends_with("does not cover entity schema paths: meta.reviewer."),
        "{one}"
    );
}

#[test]
fn a_definition_outside_the_meta_schema_is_refused_naming_the_instance_path() {
    let mut bad_strategy = notebook_document();
    set(
        &mut bad_strategy,
        "/projections/notes.list/materialization/strategy",
        json(r#""stream""#),
    );
    assert_mentions(
        &refusal(bad_strategy),
        &["/projections/notes.list/materialization/strategy"],
    );

    let mut no_namespace = notebook_document();
    remove(&mut no_namespace, "/namespace");
    assert_mentions(&refusal(no_namespace), &["namespace"]);

    let mut unknown_writer = notebook_document();
    set(
        &mut unknown_writer,
        "/collaboration/entities/Note/fields/status/writers",
        json(r#"{ "robot": "immutable" }"#),
    );
    assert_mentions(
        &refusal(unknown_writer),
        &["/collaboration/entities/Note/fields/status/writers"],
    );
}

/// Each semantic rule, provoked once, names the definition path it guards.
#[test]
fn every_semantic_rule_refuses_naming_the_definition_path() {
    type Mutation = fn(&mut Json);
    let cases: &[(&str, Mutation, &[&str])] = &[
        (
            "type name collision",
            |d| {
                let summary = d.pointer("/$defs/NoteSummary").cloned().expect("declared");
                set(d, "/$defs/noteSummary", summary);
            },
            &["$defs.noteSummary", "$defs.NoteSummary"],
        ),
        (
            "entity id",
            |d| set(d, "/entities/Task/id", json(r#""missing_id""#)),
            &["entities.Task.id", "missing_id"],
        ),
        (
            "revision field",
            |d| set(d, "/entities/Task/revision/field", json(r#""missing""#)),
            &["entities.Task.revision.field"],
        ),
        (
            "collaborative revision",
            |d| {
                set(
                    d,
                    "/entities/Note/revision",
                    json(r#"{ "kind": "contentHash", "field": "etag" }"#),
                )
            },
            &["entities.Note.authoring.kind"],
        ),
        (
            "planning not touching",
            |d| {
                push(
                    d,
                    "/entities/Task/authoring/planningMutations",
                    json(r#""note.pin""#),
                )
            },
            &["entities.Task.authoring.planningMutations", "note.pin"],
        ),
        (
            "collaboration missing",
            |d| remove(d, "/collaboration/entities/Board"),
            &["entities.Board", "collaboration.entities.Board"],
        ),
        (
            "collaboration of a non-collaborative entity",
            |d| {
                let note = d
                    .pointer("/collaboration/entities/Note")
                    .cloned()
                    .expect("declared");
                set(d, "/collaboration/entities/Task", note);
            },
            &["collaboration.entities.Task"],
        ),
        (
            "depends on",
            |d| {
                set(
                    d,
                    "/projections/notes.list/dependsOn",
                    json(r#"["Missing"]"#),
                )
            },
            &["projections.notes.list.dependsOn", "Missing"],
        ),
        (
            "touches read-only",
            |d| {
                set(
                    d,
                    "/mutations/sync.reset/touches",
                    json(r#"["SyncHealth", "Activity"]"#),
                )
            },
            &["mutations.sync.reset.touches", "Activity"],
        ),
        (
            "output name",
            |d| {
                let list = d
                    .pointer("/projections/notes.list")
                    .cloned()
                    .expect("declared");
                set(d, "/projections/note.summary", list);
            },
            &["projections.note.summary", "$defs.NoteSummary"],
        ),
        (
            "record constraints output name",
            |d| {
                set(
                    d,
                    "/$defs/NotePropertiesConstraints",
                    json(r#"{ "type": "string" }"#),
                )
            },
            &[
                "$defs.NotePropertiesConstraints",
                "$defs.NoteProperties.additionalProperties",
            ],
        ),
        (
            "command touches an unknown entity",
            |d| {
                set(
                    d,
                    "/commands/task.archiveDone/touches",
                    json(r#"["Missing"]"#),
                )
            },
            &["commands.task.archiveDone.touches", "Missing"],
        ),
        (
            "command touches read-only",
            |d| {
                set(
                    d,
                    "/commands/task.archiveDone/touches",
                    json(r#"["Task", "Activity"]"#),
                )
            },
            &["commands.task.archiveDone.touches", "Activity"],
        ),
        (
            "command result",
            |d| {
                set(
                    d,
                    "/commands/note.preview/result",
                    json(r##"{ "$ref": "#/$defs/Missing" }"##),
                )
            },
            &["commands.note.preview.result"],
        ),
        (
            "command named as a mutation",
            |d| {
                let preview = d
                    .pointer("/commands/note.preview")
                    .cloned()
                    .expect("declared");
                set(d, "/commands/note.rename", preview);
            },
            &["note.rename"],
        ),
        (
            "command output name",
            |d| {
                let preview = d
                    .pointer("/commands/note.preview")
                    .cloned()
                    .expect("declared");
                set(d, "/commands/note.summary", preview);
            },
            &["commands.note.summary", "$defs.NoteSummary"],
        ),
        (
            "update fields",
            |d| remove(d, "/projections/notes.byId/materialization/updatesField"),
            &["projections.notes.byId.materialization"],
        ),
        (
            "update contract",
            |d| {
                set(
                    d,
                    "/$defs/NoteChanges/properties/title",
                    json(r#"{ "type": "string", "minLength": 1 }"#),
                )
            },
            &["projections.notes.byId.materialization", "title"],
        ),
        (
            "omit kind",
            |d| {
                push(
                    d,
                    "/projections/notes.byId/materialization/snapshotOmitFields",
                    json(r#""kind""#),
                )
            },
            &["projections.notes.byId.materialization.snapshotOmitFields"],
        ),
        (
            "remove field",
            |d| {
                set(
                    d,
                    "/projections/notes.byId/materialization/removeField",
                    json(r#""missing""#),
                )
            },
            &["projections.notes.byId.materialization.removeField"],
        ),
        (
            "item identity",
            |d| {
                set(
                    d,
                    "/projections/notes.list/materialization/itemIdentityField",
                    json(r#""missing""#),
                )
            },
            &["projections.notes.list.materialization.itemIdentityField"],
        ),
        (
            "sequenced output",
            |d| {
                set(
                    d,
                    "/projections/activity.streams/materialization/patchOutputField",
                    json(r#""missing""#),
                )
            },
            &["projections.activity.streams.materialization.patchOutputField"],
        ),
        (
            "snapshot field",
            |d| {
                set(
                    d,
                    "/projections/tasks.board/materialization/snapshotField",
                    json(r#""missing""#),
                )
            },
            &["projections.tasks.board.materialization.snapshotField"],
        ),
        (
            "patch matches snapshot",
            |d| {
                remove(d, "/$defs/WorkspaceSettingsPatch/properties/theme");
                set(
                    d,
                    "/$defs/WorkspaceSettingsPatch/required",
                    json(r#"["kind", "scope", "revision", "shortcuts", "autosave_seconds"]"#),
                );
            },
            &["projections.workspace.current.materialization"],
        ),
        (
            "authoring projection",
            |d| {
                set(
                    d,
                    "/collaboration/entities/Note/authoringState/projection",
                    json(r#""tasks.byId""#),
                )
            },
            &["collaboration.entities.Note.authoringState.projection"],
        ),
        (
            "import mutation",
            |d| {
                set(
                    d,
                    "/collaboration/entities/Note/authoringState/importMutation",
                    json(r#""task.save""#),
                )
            },
            &["collaboration.entities.Note.authoringState.importMutation"],
        ),
        (
            "field path",
            |d| {
                let title = d
                    .pointer("/collaboration/entities/Note/fields/title")
                    .cloned()
                    .expect("declared");
                set(d, "/collaboration/entities/Note/fields/missing.path", title);
            },
            &["collaboration.entities.Note.fields.missing.path"],
        ),
        (
            "field required",
            |d| {
                set(
                    d,
                    "/collaboration/entities/Note/fields/summary/required",
                    json("true"),
                )
            },
            &["collaboration.entities.Note.fields.summary.required"],
        ),
        (
            "field its parent requires, undeclared",
            |d| {
                remove(
                    d,
                    "/collaboration/entities/Note/fields/source.url/requiredInParent",
                )
            },
            &["collaboration.entities.Note.fields.source.url.requiredInParent"],
        ),
        (
            "field its parent leaves optional, declared required there",
            |d| {
                set(
                    d,
                    "/collaboration/entities/Note/fields/meta.reviewer/requiredInParent",
                    json("true"),
                )
            },
            &["collaboration.entities.Note.fields.meta.reviewer.requiredInParent"],
        ),
        (
            "scalar key",
            |d| remove(d, "/collaboration/entities/Note/fields/title/storage/key"),
            &["collaboration.entities.Note.fields.title.storage.key"],
        ),
        (
            "sequence order",
            |d| {
                remove(
                    d,
                    "/collaboration/entities/Board/fields/columns/storage/orderContainer",
                )
            },
            &["collaboration.entities.Board.fields.columns.storage.orderContainer"],
        ),
        (
            "property text metadata",
            |d| {
                remove(
                    d,
                    "/collaboration/entities/Note/fields/body/storage/metadataKey",
                )
            },
            &["collaboration.entities.Note.fields.body.storage.metadataKey"],
        ),
        (
            "structured schema",
            |d| remove(d, "/collaboration/entities/Note/fields/layout/value/schema"),
            &["collaboration.entities.Note.fields.layout.value.schema"],
        ),
        (
            "reference with validation constraints",
            |d| {
                set(
                    d,
                    "/$defs/NoteSummary/properties/title",
                    json(r##"{ "$ref": "#/$defs/SortKey", "minLength": 1 }"##),
                )
            },
            &["$defs.NoteSummary.properties.title"],
        ),
        (
            "remote ref",
            |d| {
                set(
                    d,
                    "/$defs/NoteSummary/properties/title",
                    json(r#"{ "$ref": "other.json#/Thing" }"#),
                )
            },
            &["$defs.NoteSummary.properties.title"],
        ),
        (
            "enum values",
            |d| {
                set(
                    d,
                    "/$defs/NoteSummary/properties/title",
                    json(r#"{ "type": "string", "enum": [1] }"#),
                )
            },
            &["$defs.NoteSummary.properties.title.enum"],
        ),
        (
            "untyped",
            |d| {
                set(
                    d,
                    "/$defs/NoteSummary/properties/title",
                    json(r#"{ "description": "untyped" }"#),
                )
            },
            &["$defs.NoteSummary.properties.title"],
        ),
        (
            "open object",
            |d| set(d, "/$defs/NoteSummary/additionalProperties", json("true")),
            &["$defs.NoteSummary"],
        ),
        (
            "record with properties",
            |d| set(d, "/$defs/NoteProperties/properties", json("{}")),
            &["$defs.NoteProperties"],
        ),
        (
            "required property",
            |d| push(d, "/$defs/NoteSummary/required", json(r#""missing""#)),
            &["$defs.NoteSummary.required", "missing"],
        ),
        (
            "array items",
            |d| remove(d, "/$defs/NotesListSnapshot/properties/notes/items"),
            &["$defs.NotesListSnapshot.properties.notes"],
        ),
        (
            "schema type",
            |d| {
                set(
                    d,
                    "/$defs/NoteSummary/properties/title",
                    json(r#"{ "type": "date" }"#),
                )
            },
            &["$defs.NoteSummary.properties.title", "date"],
        ),
        (
            "default type",
            |d| set(d, "/$defs/NoteSummary/properties/title/default", json("3")),
            &["$defs.NoteSummary.properties.title.default"],
        ),
        (
            "default in enum",
            |d| {
                set(
                    d,
                    "/$defs/NoteDocument/properties/status/default",
                    json(r#""gone""#),
                )
            },
            &["$defs.NoteDocument.properties.status.default"],
        ),
        (
            "union default",
            |d| set(d, "/$defs/SortKey/default", json("true")),
            &["$defs.SortKey.default"],
        ),
        (
            "nested union",
            |d| {
                set(
                    d,
                    "/$defs/NoteSummary/properties/union",
                    json(
                        r#"{ "type": "object", "discriminator": "kind", "oneOf": { "a": { "type": "object", "additionalProperties": false, "properties": {} } } }"#,
                    ),
                )
            },
            &["$defs.NoteSummary.properties.union"],
        ),
    ];
    for (name, mutate, fragments) in cases {
        let mut document = notebook_document();
        mutate(&mut document);
        let message = refusal(document);
        for fragment in *fragments {
            assert!(
                message.contains(fragment),
                "{name}: expected {fragment:?} in the refusal: {message}"
            );
        }
    }
}

/// Some constructs pass validation but have no exact rendering in one of the
/// languages; generation refuses them rather than widening a type.
#[test]
fn a_construct_a_renderer_cannot_represent_is_refused_at_generation() {
    let config = notebook_config();
    let cases = [
        (
            "/$defs/NoteSummary/properties/nested",
            r#"{ "type": "object", "additionalProperties": false, "properties": {} }"#,
            "NoteSummary.nested",
        ),
        (
            "/$defs/NoteSummary/properties/nested",
            r#"{ "type": "object", "properties": { "kind": { "type": "string" } },
                "additionalProperties": { "type": "string" } }"#,
            "NoteSummary.nested",
        ),
        (
            "/$defs/NoteSummary/properties/sort",
            r#"{ "type": ["string", "integer"] }"#,
            "NoteSummary.sort",
        ),
        ("/$defs/Label", r#"{ "type": "string" }"#, "$defs.Label"),
    ];
    for (pointer, schema, fragment) in cases {
        let mut document = notebook_document();
        set(&mut document, pointer, json(schema));
        let definition = testing::definition(document)
            .unwrap_or_else(|error| panic!("{pointer} validates: {error}"));
        match build_outputs(&definition, &config) {
            Err(Error::Definition(message)) => assert_mentions(&message, &[fragment]),
            other => panic!("{pointer}: expected a refusal, got {other:?}"),
        }
    }
}

/// A tagged union's Rust and TypeScript renderings differ in kind rather than
/// spelling, so each way of declaring a malformed one must fail rather than
/// render something plausible.
#[test]
fn a_malformed_tagged_union_is_rejected() {
    let empty_variant = r#"{ "type": "object", "additionalProperties": false, "properties": {} }"#;
    let cases = [
        (
            "no discriminator",
            format!(r#"{{ "type": "object", "oneOf": {{ "a": {empty_variant} }} }}"#),
            "discriminator must name the property carrying the tag",
        ),
        (
            "no variants at all",
            r#"{ "type": "object", "discriminator": "kind", "oneOf": {} }"#.to_owned(),
            "at least one variant",
        ),
        (
            "a variant restating the tag",
            format!(
                r#"{{ "type": "object", "discriminator": "kind", "oneOf": {{
                    "a": {{ "type": "object", "additionalProperties": false, "properties": {{ "kind": {{ "type": "string" }} }} }},
                    "b": {empty_variant} }} }}"#
            ),
            "which the tag already carries",
        ),
        (
            "properties beside the variants",
            format!(
                r#"{{ "type": "object", "discriminator": "kind", "properties": {{}},
                    "oneOf": {{ "a": {empty_variant}, "b": {empty_variant} }} }}"#
            ),
            "may not declare properties beside its oneOf",
        ),
    ];
    for (name, schema, expected) in cases {
        let message = refusal(with_defs(&format!(r#"{{ "ProbeUnion": {schema} }}"#)));
        assert!(message.contains(expected), "{name}: {message}");
    }
}

/// A union of one is still a union: internal tagging means callers send the tag.
#[test]
fn a_single_variant_tagged_union_is_accepted() {
    validate(with_defs(
        r#"{ "ProbeUnion": {
            "type": "object", "discriminator": "kind",
            "oneOf": { "only": { "type": "object", "additionalProperties": false,
                "properties": { "title": { "type": "string" } } } } } }"#,
    ))
    .expect("a single-variant union is valid");
}

#[test]
fn a_container_inside_a_keyed_sequence_must_name_every_enclosing_identity() {
    let fields = "/collaboration/entities/Board/fields";
    for (field, key, template, missing) in [
        (
            "columns.*.cards",
            "orderContainer",
            "card_order",
            "{column_id}",
        ),
        (
            "columns.*.cards",
            "itemContainerTemplate",
            "column.{column_id}.card",
            "{card_id}",
        ),
        (
            "columns.*.cards.*.text",
            "containerTemplate",
            "card.{card_id}.text",
            "{column_id}",
        ),
    ] {
        let mut document = notebook_document();
        set(
            &mut document,
            &format!("{fields}/{field}/storage/{key}"),
            json(&format!("\"{template}\"")),
        );
        assert_mentions(
            &refusal(document),
            &[&format!("fields.{field}.storage.{key}"), missing],
        );
    }
}

#[test]
fn a_definition_named_as_the_collaborative_document_types_is_refused() {
    assert_mentions(
        &refusal(with_defs(
            r#"{ "CollaborationDocument": { "type": "object", "additionalProperties": false,
                "properties": { "label": { "type": "string" } } } }"#,
        )),
        &[
            "$defs.CollaborationDocument",
            "the generated collaborative document types",
            "CollaborationDocument",
        ],
    );
}

/// Every name the generated bindings import is taken in their module, so no
/// `$defs` entry may generate it.
#[test]
fn a_definition_named_as_a_name_the_bindings_import_is_refused() {
    for name in [
        "CollaborationReplicaDocument",
        "AuthoringPlan",
        "CollaborationPlanTextFieldPath",
        "ClerkenwellCollaborationEntityPlan",
        "AuthoringPolicyKind",
        "CollaborationFieldPlan",
        "GeneratedMutationSpec",
        "ActorKind",
        "Serialize",
        "JsonValue",
    ] {
        assert_mentions(
            &refusal(with_defs(&format!(
                r#"{{ "{name}": {{ "type": "object", "additionalProperties": false,
                    "properties": {{ "label": {{ "type": "string" }} }} }} }}"#
            ))),
            &[&format!("$defs.{name}"), "the generated bindings' import"],
        );
    }
}

#[test]
fn a_definition_may_declare_no_commands() {
    let mut document = notebook_document();
    remove(&mut document, "/commands");
    validate(document).expect("a definition without commands is valid");
}

#[test]
fn a_command_may_touch_no_entity() {
    let mut document = notebook_document();
    remove(&mut document, "/commands/task.archiveDone/touches");
    validate(document).expect("a command that touches nothing is valid");
}

/// An open object: declared members beside other members of a typed schema.
fn open_object(properties: &str, required: &str, other_members: &str) -> String {
    format!(
        r#"{{ "type": "object", "properties": {properties}, "required": {required},
            "additionalProperties": {other_members} }}"#
    )
}

#[test]
fn an_open_object_declares_members_that_are_values_of_its_other_members() {
    let cases = [
        (
            r#"{ "kind": { "type": "string" }, "size": { "type": "integer" } }"#,
            r#"{ "type": "json" }"#,
        ),
        (
            r#"{ "kind": { "type": "string", "enum": ["a", "b"] } }"#,
            r#"{ "type": "string" }"#,
        ),
        (
            r#"{ "count": { "type": "integer" } }"#,
            r#"{ "type": "number" }"#,
        ),
        (
            r#"{ "count": { "type": "integer", "minimum": 2, "maximum": 4 } }"#,
            r#"{ "type": "integer", "minimum": 0, "maximum": 10 }"#,
        ),
        (
            r#"{ "sizes": { "type": "array", "items": { "type": "integer" } } }"#,
            r#"{ "type": "array", "items": { "type": "number" } }"#,
        ),
        (
            r#"{ "labels": { "type": "object", "additionalProperties": { "type": "string", "const": "x" } } }"#,
            r#"{ "type": "object", "additionalProperties": { "type": "string" } }"#,
        ),
        (
            r##"{ "summary": { "$ref": "#/$defs/NoteSummary" } }"##,
            r##"{ "$ref": "#/$defs/NoteSummary" }"##,
        ),
        (
            r#"{ "kind": { "type": "string" } }"#,
            r#"{ "type": ["string", "boolean"] }"#,
        ),
    ];
    for (properties, other_members) in cases {
        validate(with_defs(&format!(
            r#"{{ "ProbeOpen": {} }}"#,
            open_object(properties, "[]", other_members)
        )))
        .unwrap_or_else(|error| panic!("{properties} beside {other_members}: {error}"));
    }
}

#[test]
fn an_open_object_is_refused_where_its_members_cannot_be_held() {
    let cases = [
        (
            open_object(r#"{ "count": { "type": "integer" } }"#, "[]", r#"{ "type": "string" }"#),
            "$defs.ProbeOpen.properties.count is not a value of the object's additionalProperties schema",
        ),
        (
            open_object(r#"{ "ratio": { "type": "number" } }"#, "[]", r#"{ "type": "integer" }"#),
            "$defs.ProbeOpen.properties.ratio is not a value",
        ),
        (
            open_object(r#"{ "kind": { "type": "string" } }"#, "[]", r#"{ "type": "string", "enum": ["a"] }"#),
            "$defs.ProbeOpen.properties.kind is not a value",
        ),
        (
            open_object(r#"{ "extra": { "type": "json" } }"#, "[]", r#"{ "type": "string" }"#),
            "$defs.ProbeOpen.properties.extra is not a value",
        ),
        (
            open_object(
                r##"{ "summary": { "$ref": "#/$defs/NoteSummary" } }"##,
                "[]",
                r##"{ "$ref": "#/$defs/NoteDetail" }"##,
            ),
            "$defs.ProbeOpen.properties.summary is not a value",
        ),
        (
            open_object(r#"{ "kind": { "type": "string" } }"#, r#"["kind", "colour"]"#, r#"{ "type": "string" }"#),
            "$defs.ProbeOpen.required references unknown property \"colour\"",
        ),
        (
            open_object("{}", "[]", r#"{ "type": "string" }"#),
            "$defs.ProbeOpen declares no properties beside additionalProperties",
        ),
        (
            open_object(r#"{ "otherMembers": { "type": "string" } }"#, "[]", r#"{ "type": "string" }"#),
            "$defs.ProbeOpen.properties.otherMembers is named as the Rust field holding the other members",
        ),
        (
            r#"{ "type": "object", "required": ["kind"], "additionalProperties": { "type": "string" } }"#
                .to_owned(),
            "$defs.ProbeOpen.required may name only members its properties declare",
        ),
        (
            format!(
                r#"{{ "type": "object", "discriminator": "tag", "oneOf": {{ "a": {} }} }}"#,
                open_object(r#"{ "kind": { "type": "string" } }"#, "[]", r#"{ "type": "string" }"#)
            ),
            "$defs.ProbeOpen.oneOf.a must be a closed object",
        ),
    ];
    for (schema, expected) in cases {
        assert_mentions(
            &refusal(with_defs(&format!(r#"{{ "ProbeOpen": {schema} }}"#))),
            &[expected],
        );
    }
}

/// A field stores an open object whole: its members, declared or not, are one
/// value, so no field path reaches into it, and a collaborative entity's
/// schema declares its members as fields.
#[test]
fn a_collaboration_field_stores_an_open_object_whole() {
    let mut document = notebook_document();
    let theme = document
        .pointer("/collaboration/entities/Note/fields/appearance")
        .cloned()
        .expect("the notebook stores an appearance");
    set(
        &mut document,
        "/collaboration/entities/Note/fields/appearance.theme",
        theme,
    );
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Note.fields.appearance.theme does not resolve in NoteDocument"],
    );

    let mut document = notebook_document();
    remove(
        &mut document,
        "/collaboration/entities/Note/fields/appearance",
    );
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Note.fields does not cover entity schema paths: appearance."],
    );

    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/NoteDocument/additionalProperties",
        json(r#"{ "type": "json" }"#),
    );
    assert_mentions(
        &refusal(document),
        &["entities.Note.schema must be a closed object"],
    );
}

/// A union's constraints bind the members it admits: a bound on numbers
/// leaves a string member alone and holds an integer member to it.
#[test]
fn an_open_object_admits_a_union_member_only_within_the_union_constraints() {
    let bounded = r#"{ "type": ["string", "integer"], "minimum": 10, "maximum": 20 }"#;
    let probe = |member: &str| {
        with_defs(&format!(
            r##"{{ "ProbeBounded": {bounded}, "ProbeOpen": {} }}"##,
            open_object(
                &format!(r#"{{ "member": {member} }}"#),
                "[]",
                r##"{ "$ref": "#/$defs/ProbeBounded" }"##
            )
        ))
    };
    for admitted in [
        r#"{ "type": "string" }"#,
        r#"{ "type": "integer", "minimum": 12, "maximum": 20 }"#,
        r#"{ "type": ["string", "integer"], "minimum": 10, "maximum": 15 }"#,
    ] {
        validate(probe(admitted)).unwrap_or_else(|error| panic!("{admitted}: {error}"));
    }
    for refused in [
        r#"{ "type": "integer" }"#,
        r#"{ "type": "integer", "minimum": 5, "maximum": 15 }"#,
        r#"{ "type": ["string", "integer"], "minimum": 10 }"#,
        r#"{ "type": "boolean" }"#,
    ] {
        assert_mentions(
            &refusal(probe(refused)),
            &["$defs.ProbeOpen.properties.member is not a value"],
        );
    }
}

/// A member that enumerates its values is admitted when each value meets the
/// other members' bounds.
#[test]
fn an_open_object_admits_an_enumerated_member_by_its_values() {
    let probe = |member: &str, other_members: &str| {
        with_defs(&format!(
            r#"{{ "ProbeOpen": {} }}"#,
            open_object(&format!(r#"{{ "member": {member} }}"#), "[]", other_members)
        ))
    };
    for (member, other_members) in [
        (
            r#"{ "type": "string", "enum": ["ab", "abc"] }"#,
            r#"{ "type": "string", "minLength": 2 }"#,
        ),
        (
            r#"{ "type": "string", "const": "abc" }"#,
            r#"{ "type": "string", "minLength": 3 }"#,
        ),
        (
            r#"{ "type": "integer", "const": 12 }"#,
            r#"{ "type": "integer", "minimum": 10, "maximum": 12 }"#,
        ),
        (
            r#"{ "type": "string", "enum": ["x"] }"#,
            r#"{ "type": "string", "const": "x" }"#,
        ),
        (
            r#"{ "type": "string", "const": "x" }"#,
            r#"{ "type": "string", "const": "x" }"#,
        ),
    ] {
        validate(probe(member, other_members))
            .unwrap_or_else(|error| panic!("{member} beside {other_members}: {error}"));
    }
    for (member, other_members) in [
        (
            r#"{ "type": "string", "enum": ["a", "abc"] }"#,
            r#"{ "type": "string", "minLength": 2 }"#,
        ),
        (
            r#"{ "type": "integer", "const": 13 }"#,
            r#"{ "type": "integer", "maximum": 12 }"#,
        ),
        (
            r#"{ "type": "string", "enum": ["x", "y"] }"#,
            r#"{ "type": "string", "const": "x" }"#,
        ),
        (
            r#"{ "type": "string" }"#,
            r#"{ "type": "string", "const": "x" }"#,
        ),
    ] {
        assert_mentions(
            &refusal(probe(member, other_members)),
            &["$defs.ProbeOpen.properties.member is not a value"],
        );
    }
}

/// A structural keyword shapes only the type it belongs to; on another type
/// it would shape nothing either binding renders.
#[test]
fn a_structural_keyword_is_refused_on_a_type_it_does_not_apply_to() {
    let cases = [
        (
            r#"{ "type": "string", "additionalProperties": { "type": "string", "minLength": 2 } }"#,
            "$defs.ProbeScalar.additionalProperties applies only to object schemas",
        ),
        (
            r#"{ "type": "integer", "properties": {} }"#,
            "$defs.ProbeScalar.properties applies only to object schemas",
        ),
        (
            r#"{ "type": "boolean", "required": [] }"#,
            "$defs.ProbeScalar.required applies only to object schemas",
        ),
        (
            r#"{ "type": "number", "items": { "type": "string" } }"#,
            "$defs.ProbeScalar.items applies only to array schemas",
        ),
        (
            r#"{ "type": ["string", "integer"], "additionalProperties": { "type": "string" } }"#,
            "$defs.ProbeScalar.additionalProperties applies only to object schemas",
        ),
        (
            r#"{ "type": "array", "items": { "type": "string" }, "properties": {} }"#,
            "$defs.ProbeScalar.properties applies only to object schemas",
        ),
        (
            r#"{ "type": "object", "additionalProperties": false, "properties": {}, "items": { "type": "string" } }"#,
            "$defs.ProbeScalar.items applies only to array schemas",
        ),
    ];
    for (schema, expected) in cases {
        assert_mentions(
            &refusal(with_defs(&format!(r#"{{ "ProbeScalar": {schema} }}"#))),
            &[expected],
        );
    }
}

/// A keyed sequence keeps each item's members in containers of their own, so
/// its items are closed objects: an open object's or a record's members are
/// one value, which no item field addresses.
#[test]
fn a_keyed_sequence_holds_closed_objects() {
    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/BoardColumn/additionalProperties",
        json(r#"{ "type": "json" }"#),
    );
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Board.fields.columns is a keyed sequence, whose items must be closed objects"],
    );
    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/BoardDocument/properties/columns/items",
        json(r#"{ "type": "object", "additionalProperties": { "type": "string" } }"#),
    );
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Board.fields.columns is a keyed sequence, whose items must be closed objects"],
    );
}

/// A union's validation keywords are checked against its member types: a
/// bound needs a numeric member, `minLength` a string member, and `const` a
/// value of one of its types.
#[test]
fn a_union_validation_keyword_is_checked_against_its_types() {
    let refused = [
        (
            r#"{ "type": ["string", "integer"], "minimum": "x" }"#,
            "$defs.ProbeUnion.minimum requires a finite numeric bound on a numeric schema",
        ),
        (
            r#"{ "type": ["string", "boolean"], "maximum": 3 }"#,
            "$defs.ProbeUnion.maximum requires a finite numeric bound on a numeric schema",
        ),
        (
            r#"{ "type": ["integer", "number"], "minimum": 3, "maximum": 2 }"#,
            "$defs.ProbeUnion.minimum exceeds maximum",
        ),
        (
            r#"{ "type": ["integer", "boolean"], "minLength": 1 }"#,
            "$defs.ProbeUnion.minLength is supported only on string schemas",
        ),
        (
            r#"{ "type": ["string", "integer"], "minLength": -1 }"#,
            "$defs.ProbeUnion.minLength must be a non-negative integer",
        ),
        (
            r#"{ "type": ["string", "integer"], "const": true }"#,
            "$defs.ProbeUnion.const must be a value of one of its types",
        ),
        (
            r#"{ "type": ["boolean", "integer"], "const": 1.5 }"#,
            "$defs.ProbeUnion.const must be a value of one of its types",
        ),
    ];
    for (schema, expected) in refused {
        assert_mentions(
            &refusal(with_defs(&format!(r#"{{ "ProbeUnion": {schema} }}"#))),
            &[expected],
        );
    }
    for schema in [
        r#"{ "type": ["string", "integer"], "minimum": 0, "minLength": 1 }"#,
        r#"{ "type": ["string", "integer"], "const": 3 }"#,
        r#"{ "type": ["boolean", "number"], "const": 1.5 }"#,
    ] {
        validate(with_defs(&format!(r#"{{ "ProbeUnion": {schema} }}"#)))
            .unwrap_or_else(|error| panic!("{schema}: {error}"));
    }
}

/// A `$defs` entry that is a reference names the schema at the end of its
/// chain of references; a chain that returns to an entry names none.
#[test]
fn a_chain_of_references_ends_at_a_schema() {
    let message = refusal(with_defs(
        r##"{ "ProbeA": { "$ref": "#/$defs/ProbeB" }, "ProbeB": { "$ref": "#/$defs/ProbeA" } }"##,
    ));
    assert!(message.contains("is a cycle of references"), "{message}");

    // A keyed sequence's closed item reached through two references.
    let mut document = with_defs(
        r##"{ "ProbeColumn": { "$ref": "#/$defs/ProbeColumnAlias" },
            "ProbeColumnAlias": { "$ref": "#/$defs/BoardColumn" } }"##,
    );
    set(
        &mut document,
        "/$defs/BoardDocument/properties/columns/items",
        json(r##"{ "$ref": "#/$defs/ProbeColumn" }"##),
    );
    validate(document).expect("a closed item reached through aliases is a keyed sequence's item");
}

/// A member whose references end at the definition the other members' do is
/// a value of it, whichever aliases each passes through.
#[test]
fn an_open_object_admits_a_member_reaching_its_other_members_type_through_aliases() {
    validate(with_defs(
        r##"{
            "ProbeTarget": { "type": "object", "additionalProperties": false,
                "properties": { "label": { "type": "string" } } },
            "ProbeAlias": { "$ref": "#/$defs/ProbeTarget" },
            "ProbeOpen": { "type": "object",
                "properties": { "member": { "$ref": "#/$defs/ProbeAlias" } },
                "additionalProperties": { "$ref": "#/$defs/ProbeTarget" } }
        }"##,
    ))
    .expect("a member behind an alias of the other members' type is admitted");
}

/// A constant is a value of its schema only when it meets the schema's
/// bounds; one that does not leaves the schema no values.
#[test]
fn a_constant_meets_its_bounds() {
    for (schema, expected) in [
        (
            r#"{ "type": "string", "const": "x", "minLength": 2 }"#,
            "$defs.ProbeConstant.const does not meet its minLength",
        ),
        (
            r#"{ "type": "integer", "const": 1, "minimum": 2 }"#,
            "$defs.ProbeConstant.const does not meet its minimum",
        ),
        (
            r#"{ "type": ["string", "number"], "const": 9.5, "maximum": 9 }"#,
            "$defs.ProbeConstant.const does not meet its maximum",
        ),
    ] {
        assert_mentions(
            &refusal(with_defs(&format!(r#"{{ "ProbeConstant": {schema} }}"#))),
            &[expected],
        );
    }
    for schema in [
        r#"{ "type": "string", "const": "xy", "minLength": 2 }"#,
        r#"{ "type": ["string", "integer"], "const": "x", "minimum": 2 }"#,
    ] {
        validate(with_defs(&format!(r#"{{ "ProbeConstant": {schema} }}"#)))
            .unwrap_or_else(|error| panic!("{schema}: {error}"));
    }
}

/// An enumerated member's values are those its own bounds leave it, for
/// the other members' bounds; both bindings render every value it lists, so
/// each is one of the other members' enumerated values.
#[test]
fn an_open_object_admits_an_enumerated_member_by_the_values_its_bounds_leave() {
    validate(with_defs(&format!(
        r#"{{ "ProbeOpen": {} }}"#,
        open_object(
            r#"{ "member": { "type": "string", "enum": ["a", "valid"], "minLength": 2 } }"#,
            "[]",
            r#"{ "type": "string", "minLength": 2 }"#,
        )
    )))
    .expect("the member's bounds leave it only values of the other members");
    assert_mentions(
        &refusal(with_defs(&format!(
            r#"{{ "ProbeOpen": {} }}"#,
            open_object(
                r#"{ "member": { "type": "string", "enum": ["a", "valid"], "minLength": 2 } }"#,
                "[]",
                r#"{ "type": "string", "enum": ["valid"] }"#,
            )
        ))),
        &["$defs.ProbeOpen.properties.member is not a value"],
    );
}

/// A member that lists its values is admitted by the type of those values,
/// whatever wider type it declares.
#[test]
fn an_open_object_admits_a_listed_member_by_the_types_of_its_values() {
    let probe = |member: &str| {
        with_defs(&format!(
            r#"{{ "ProbeOpen": {} }}"#,
            open_object(
                &format!(r#"{{ "member": {member} }}"#),
                "[]",
                r#"{ "type": "integer" }"#
            )
        ))
    };
    validate(probe(r#"{ "type": "number", "const": 1 }"#))
        .expect("a number member whose one value is an integer");
    assert_mentions(
        &refusal(probe(r#"{ "type": "number", "const": 1.5 }"#)),
        &["$defs.ProbeOpen.properties.member is not a value"],
    );
}

/// A `default` beside a reference is a value of the schema its chain of
/// references ends at.
#[test]
fn a_default_beside_a_reference_is_checked_at_the_end_of_its_chain() {
    let probe = |default: &str| {
        with_defs(&format!(
            r##"{{
                "ProbeTarget": {{ "type": "string", "enum": ["valid", "other"] }},
                "ProbeAlias": {{ "$ref": "#/$defs/ProbeTarget" }},
                "ProbeHolder": {{ "type": "object", "additionalProperties": false,
                    "properties": {{ "level": {{ "$ref": "#/$defs/ProbeAlias", "default": {default} }} }} }}
            }}"##
        ))
    };
    validate(probe(r#""valid""#)).expect("a default of the aliased enum");
    assert_mentions(
        &refusal(probe(r#""nope""#)),
        &["default must be one of the values its enum admits"],
    );
}

/// An integer schema's bounds are the integers they leave: a fractional
/// bound is its ceiling as a minimum and its floor as a maximum.
#[test]
fn an_open_object_compares_integer_bounds_by_the_integers_they_leave() {
    let probe = |member: &str, other_members: &str| {
        with_defs(&format!(
            r#"{{ "ProbeOpen": {} }}"#,
            open_object(&format!(r#"{{ "member": {member} }}"#), "[]", other_members)
        ))
    };
    for (member, other_members) in [
        (
            r#"{ "type": "integer", "minimum": 1.1 }"#,
            r#"{ "type": "integer", "minimum": 1.5 }"#,
        ),
        (
            r#"{ "type": "integer", "minimum": 1.1 }"#,
            r#"{ "type": "number", "minimum": 1.5 }"#,
        ),
        (
            r#"{ "type": "integer", "maximum": 2.9 }"#,
            r#"{ "type": "integer", "maximum": 2.5 }"#,
        ),
    ] {
        validate(probe(member, other_members))
            .unwrap_or_else(|error| panic!("{member} beside {other_members}: {error}"));
    }
    assert_mentions(
        &refusal(probe(
            r#"{ "type": "number", "minimum": 1.1 }"#,
            r#"{ "type": "number", "minimum": 1.5 }"#,
        )),
        &["$defs.ProbeOpen.properties.member is not a value"],
    );
}

/// TypeScript renders a constant member as its type and an enumerated one as
/// its values, so beside enumerated other members only an enumerated member
/// is assignable to them.
#[test]
fn a_constant_member_is_refused_beside_enumerated_other_members() {
    assert_mentions(
        &refusal(with_defs(&format!(
            r#"{{ "ProbeOpen": {} }}"#,
            open_object(
                r#"{ "member": { "type": "string", "const": "x" } }"#,
                "[]",
                r#"{ "type": "string", "enum": ["x"] }"#,
            )
        ))),
        &["$defs.ProbeOpen.properties.member is not a value"],
    );
}

/// The Rust model holds an integer as an `i64`, so an integer constant is
/// one; a union with a number member holds a wider one.
#[test]
fn an_integer_constant_is_an_i64() {
    assert_mentions(
        &refusal(with_defs(
            r#"{ "ProbeConstant": { "type": "integer", "const": 1e20 } }"#,
        )),
        &["$defs.ProbeConstant.const must be an integer an i64 holds"],
    );
    validate(with_defs(
        r#"{ "ProbeConstant": { "type": ["integer", "number"], "const": 1e20 } }"#,
    ))
    .expect("a union's number member holds the constant");
    assert_mentions(
        &refusal(with_defs(
            r#"{ "ProbeConstant": { "type": ["integer", "string"], "const": 1e20 } }"#,
        )),
        &["$defs.ProbeConstant.const must be an integer an i64 holds"],
    );
}

/// A field holding an object stores it whole, as `structuredJson` over that
/// object's schema.
#[test]
fn a_field_holding_an_object_is_structured_json_over_its_schema() {
    let mut document = notebook_document();
    set(
        &mut document,
        "/collaboration/entities/Note/fields/appearance",
        json(
            r#"{ "storage": { "kind": "scalar", "container": "appearance", "key": "value" },
            "value": { "codec": "string" }, "required": true, "conflict": "merge" }"#,
        ),
    );
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Note.fields.appearance holds an object, so its codec is structuredJson over that object's schema"],
    );
}

/// A field's value schema resolves to the schema the entity schema declares at
/// its path, or, for a structured list, to the schema of one item of the array
/// there.
#[test]
fn a_value_schema_is_the_entity_schema_at_its_path() {
    let cases = [
        (
            "appearance",
            "NoteExtras",
            "is #/$defs/NoteExtras, but NoteDocument declares appearance as #/$defs/NoteAppearance",
        ),
        (
            "layout",
            "NoteProperties",
            "is #/$defs/NoteProperties, but NoteDocument declares layout as #/$defs/NoteLayout",
        ),
        (
            "attachments",
            "AttachmentPreview",
            "is #/$defs/AttachmentPreview, but NoteDocument declares an item of attachments as #/$defs/Attachment",
        ),
    ];
    for (field, schema, message) in cases {
        let mut document = notebook_document();
        set(
            &mut document,
            &format!("/collaboration/entities/Note/fields/{field}/value/schema"),
            json(&format!(r##"{{ "$ref": "#/$defs/{schema}" }}"##)),
        );
        assert_mentions(
            &refusal(document),
            &[&format!(
                "collaboration.entities.Note.fields.{field}.value.schema {message}."
            )],
        );
    }

    let mut document = notebook_document();
    set(
        &mut document,
        "/collaboration/entities/Note/fields/layout/storage/kind",
        json(r#""structuredList""#),
    );
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Note.fields.layout.value.schema is the schema of one item of a structured list, but NoteDocument declares no array at layout."],
    );

    // An inline schema is no `$defs` entry, so no reference names it.
    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/NoteDocument/properties/attachments/items",
        json(r#"{ "type": "object", "additionalProperties": { "type": "string" } }"#),
    );
    assert_mentions(
        &refusal(document),
        &["collaboration.entities.Note.fields.attachments.value.schema is #/$defs/Attachment, but NoteDocument declares an item of attachments as an inline schema."],
    );

    // An alias generates the type it names, so it describes the same values.
    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/NoteAppearanceAlias",
        json(r##"{ "$ref": "#/$defs/NoteAppearance" }"##),
    );
    set(
        &mut document,
        "/$defs/NoteDocument/properties/appearance",
        json(r##"{ "$ref": "#/$defs/NoteAppearanceAlias" }"##),
    );
    validate(document).expect("the value schema resolves to the declared schema");
}

/// Each member of an object is a Rust field of its own.
#[test]
fn members_generating_one_rust_field_are_refused() {
    let members = r#"{ "file_name": { "type": "string" }, "fileName": { "type": "string" } }"#;
    let cases = [
        (
            format!(
                r#"{{ "ProbeAttachment": {{ "type": "object", "additionalProperties": false,
                "properties": {members} }} }}"#
            ),
            "$defs.ProbeAttachment",
        ),
        (
            format!(
                r#"{{ "ProbeHolder": {{ "type": "object", "additionalProperties": false,
                "properties": {{ "attachment": {{ "type": "object", "additionalProperties": false,
                "properties": {members} }} }} }} }}"#
            ),
            "$defs.ProbeHolder.properties.attachment",
        ),
        (
            format!(
                r#"{{ "ProbeUnion": {{ "type": "object", "discriminator": "kind",
                "oneOf": {{ "file": {{ "type": "object", "additionalProperties": false,
                "properties": {members} }} }} }} }}"#
            ),
            "$defs.ProbeUnion.oneOf.file",
        ),
    ];
    for (schema, context) in cases {
        assert_mentions(
            &refusal(with_defs(&schema)),
            &[&format!(
                "{context} declares \"file_name\" and \"fileName\", which both generate the Rust field file_name."
            )],
        );
    }
}

/// Each tag of a tagged union is a Rust variant of its own.
#[test]
fn tags_generating_one_rust_variant_are_refused() {
    assert_mentions(
        &refusal(with_defs(
            r#"{ "ProbeUnion": { "type": "object", "discriminator": "kind", "oneOf": {
            "a-b": { "type": "object", "additionalProperties": false, "properties": {} },
            "a_b": { "type": "object", "additionalProperties": false, "properties": {} } } } }"#,
        )),
        &["$defs.ProbeUnion.oneOf tags \"a-b\" and \"a_b\" both generate the Rust variant AB."],
    );
}

/// Each value of a string enum is a Rust variant of its own.
#[test]
fn enum_values_generating_one_rust_variant_are_refused() {
    for schema in [
        r#"{ "ProbeLevel": { "type": "string", "enum": ["a-b", "a_b"] } }"#,
        r#"{ "ProbeOpen": { "type": "object",
            "properties": { "first": { "type": "string", "enum": ["a-b"] } },
            "additionalProperties": { "type": "string", "enum": ["a-b", "a_b"] } } }"#,
    ] {
        assert_mentions(
            &refusal(with_defs(schema)),
            &["enum values \"a-b\" and \"a_b\" both generate the Rust variant AB"],
        );
    }
}

/// An enum whose bounds exclude every value it lists has no values.
#[test]
fn an_enum_with_no_value_within_its_bounds_is_refused() {
    assert_mentions(
        &refusal(with_defs(
            r#"{ "ProbeLevel": { "type": "string", "enum": ["a"], "minLength": 2 } }"#,
        )),
        &["$defs.ProbeLevel.enum has no value that meets its bounds"],
    );
    assert_mentions(
        &refusal(with_defs(&format!(
            r#"{{ "ProbeOpen": {} }}"#,
            open_object(
                r#"{ "member": { "type": "string", "enum": ["a"], "minLength": 2 } }"#,
                "[]",
                r#"{ "type": "string" }"#,
            )
        ))),
        &["$defs.ProbeOpen.properties.member.enum has no value that meets its bounds"],
    );
    validate(with_defs(
        r#"{ "ProbeLevel": { "type": "string", "enum": ["a", "valid"], "minLength": 2 } }"#,
    ))
    .expect("an enum keeping a value within its bounds");
}

/// Whether a bound is an integer's is decided by a union's numeric types: a
/// string alternative is not bounded.
#[test]
fn an_open_object_rounds_the_integer_bounds_of_a_mixed_union() {
    validate(with_defs(&format!(
        r##"{{ "ProbeOthers": {{ "type": ["number", "string"], "minimum": 1.5 }},
            "ProbeOpen": {} }}"##,
        open_object(
            r#"{ "member": { "type": ["integer", "string"], "minimum": 1.1 } }"#,
            "[]",
            r##"{ "$ref": "#/$defs/ProbeOthers" }"##,
        )
    )))
    .expect("the member's numbers begin at 2");
}

/// TypeScript renders a member by its declared types, its constant aside, so
/// each declared type is one the other members admit.
#[test]
fn an_open_object_refuses_a_member_whose_declared_types_the_others_do_not_admit() {
    assert_mentions(
        &refusal(with_defs(&format!(
            r##"{{ "ProbeChoice": {{ "type": ["integer", "string"], "const": 1 }},
                "ProbeOpen": {} }}"##,
            open_object(
                r##"{ "member": { "$ref": "#/$defs/ProbeChoice" } }"##,
                "[]",
                r#"{ "type": "integer" }"#,
            )
        ))),
        &["$defs.ProbeOpen.properties.member is not a value"],
    );
}

/// Each projection, mutation and command is an operation named
/// `<namespace>.<name>`, in the MCP tool-name alphabet and its bound.
#[test]
fn an_operation_name_outside_the_tool_name_alphabet_is_refused() {
    let longest = "x".repeat(128 - "notebook.".len());
    for (name, accepted) in [
        ("card.shuffle-all_v2", true),
        (longest.as_str(), true),
        ("card shuffle", false),
        ("card.shufflé", false),
        (&format!("{longest}x"), false),
    ] {
        let mut document = notebook_document();
        set(
            &mut document,
            &format!("/commands/{name}"),
            json(
                r##"{ "description": "Shuffles the cards.",
                      "params": { "$ref": "#/$defs/NotePreviewParams" },
                      "result": { "$ref": "#/$defs/NotePreviewResult" } }"##,
            ),
        );
        if accepted {
            validate(document).unwrap_or_else(|error| panic!("{name:?} is refused: {error}"));
        } else {
            assert_mentions(
                &refusal(document),
                &[
                    &format!("commands.{name} names the operation \"notebook.{name}\""),
                    "at most 128 ASCII letters, digits",
                ],
            );
        }
    }
}

/// An operation's input is an object, so its params schema is one.
#[test]
fn params_that_are_not_an_object_are_refused() {
    for section in [
        "projections/notes.list",
        "mutations/note.create",
        "commands/note.preview",
    ] {
        let mut document = with_defs(r#"{ "Label": { "type": "string" } }"#);
        set(
            &mut document,
            &format!("/{section}/params"),
            json(r##"{ "$ref": "#/$defs/Label" }"##),
        );
        let context = section.replace('/', ".");
        assert_mentions(
            &refusal(document),
            &[&format!(
                "{context}.params must reference a schema of type \"object\""
            )],
        );
    }
}

/// Each projection, mutation and command describes what it does, for a
/// caller deciding whether to call it.
#[test]
fn an_operation_without_a_description_is_refused() {
    for entry in [
        "/projections/notes.list",
        "/mutations/note.create",
        "/commands/note.preview",
    ] {
        let mut document = notebook_document();
        remove(&mut document, &format!("{entry}/description"));
        assert_mentions(&refusal(document), &[entry, "description"]);
        for blank in ["", " \n\t"] {
            let mut document = notebook_document();
            set(
                &mut document,
                &format!("{entry}/description"),
                Json::String(blank.to_owned()),
            );
            assert_mentions(&refusal(document), &[&format!("{entry}/description")]);
        }
    }
}

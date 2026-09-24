//! A definition outside what the generator can represent exactly is refused,
//! and the refusal names the definition path at fault.

mod common;

use clerkenwell_codegen::json::Json;
use clerkenwell_codegen::{build_outputs, Definition, Error};
use common::{json, merge, notebook_config, notebook_document, push, refusal, remove, set};

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
/// constraints reach the Rust output only as `schemars` attributes.
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
        let definition = Definition::from_json(document, &config.project).expect("valid");
        build_outputs(&definition, &config).expect("renders")
    };
    let with = render(constrained);
    let without = render(unconstrained);

    assert_eq!(with.typescript_model, without.typescript_model);
    assert_eq!(with.typescript_mnemonic, without.typescript_mnemonic);
    assert_ne!(with.rust_model, without.rust_model);
    let types_only = |rust: &str| -> Vec<String> {
        rust.lines()
            .filter(|line| !line.trim_start().starts_with("#[schemars("))
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
        &["entities.Note.authoring.lifecycleMutations references mutation \"note.rename\" that does not touch Note"],
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

/// A configured leaf schema is stored whole, so it is one path to cover; any
/// other object schema is covered property by property.
#[test]
fn a_configured_leaf_schema_is_one_collaboration_path() {
    let mut document = notebook_document();
    remove(&mut document, "/collaboration/entities/Note/fields/body");
    let config = notebook_config();
    assert!(config
        .project
        .collaboration_leaf_schemas
        .iter()
        .any(|leaf| leaf == "MimeValue"));
    let expect_refusal = |project| match Definition::from_json(document.clone(), &project) {
        Err(Error::Definition(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    };

    let as_leaf = expect_refusal(config.project.clone());
    assert!(
        as_leaf.ends_with("does not cover entity schema paths: body."),
        "{as_leaf}"
    );

    let mut project = config.project;
    project.collaboration_leaf_schemas.clear();
    let as_object = expect_refusal(project);
    assert_mentions(&as_object, &["body.$mime", "body.value"]);
}

#[test]
fn a_session_owning_entity_needs_the_configured_session_mnemonic() {
    let config = notebook_config();

    let mut unnamed = config.project.clone();
    unnamed.authoring_session_mnemonic = None;
    match Definition::from_json(notebook_document(), &unnamed) {
        Err(Error::Definition(message)) => assert_mentions(
            &message,
            &["entities.Note.authoring.kind", "authoringSessionMnemonic"],
        ),
        other => panic!("expected a refusal, got {other:?}"),
    }

    let mut document = notebook_document();
    let session = config
        .project
        .authoring_session_mnemonic
        .clone()
        .expect("the example names its session mnemonic");
    remove(&mut document, &format!("/mnemonic/{session}"));
    assert_mentions(
        &refusal(document),
        &[&format!("requires mnemonic.{session}.")],
    );
}

#[test]
fn every_configured_name_must_be_declared_by_the_definition() {
    type Configure = fn(&mut clerkenwell_codegen::Project);
    let cases: [(Configure, &[&str]); 3] = [
        (
            |project| project.authoring_session_mnemonic = Some("absent".to_owned()),
            &["mnemonic.absent"],
        ),
        (
            |project| project.collaboration_leaf_schemas.push("Absent".to_owned()),
            &["collaborationLeafSchemas", "\"Absent\""],
        ),
        (
            |project| {
                project
                    .entity_diagnostics
                    .insert("Absent".to_owned(), vec!["diagnostic".to_owned()]);
            },
            &["entityDiagnostics", "\"Absent\""],
        ),
    ];
    let config = notebook_config();
    for (configure, fragments) in cases {
        let mut project = config.project.clone();
        configure(&mut project);
        match Definition::from_json(notebook_document(), &project) {
            Err(Error::Definition(message)) => assert_mentions(&message, fragments),
            other => panic!("{fragments:?}: expected a refusal, got {other:?}"),
        }
    }
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
}

/// Each semantic rule, provoked once, names the definition path it guards.
#[test]
fn every_semantic_rule_refuses_naming_the_definition_path() {
    type Mutation = fn(&mut Json);
    let cases: &[(&str, Mutation, &[&str])] = &[
        (
            "reader version",
            |d| set(d, "/compatibility/minimumReaderVersion", json("9")),
            &["compatibility.minimumReaderVersion"],
        ),
        (
            "collaboration writer version",
            |d| {
                set(
                    d,
                    "/collaboration/compatibility/minimumWriterVersion",
                    json("9"),
                )
            },
            &["collaboration.compatibility.minimumWriterVersion"],
        ),
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
            "content mutation",
            |d| {
                set(
                    d,
                    "/entities/Task/authoring/contentMutation",
                    json(r#""note.pin""#),
                )
            },
            &["entities.Task.authoring.contentMutation"],
        ),
        (
            "content and lifecycle",
            |d| {
                push(
                    d,
                    "/entities/Task/authoring/lifecycleMutations",
                    json(r#""task.save""#),
                )
            },
            &["entities.Task.authoring", "task.save"],
        ),
        (
            "classified twice",
            |d| {
                push(
                    d,
                    "/entities/Task/authoring/commandMutations",
                    json(r#""task.schedule""#),
                )
            },
            &["entities.Task.authoring", "task.schedule"],
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
            "mnemonic schema",
            |d| {
                set(
                    d,
                    "/mnemonic/noteSelection/schema",
                    json(r##"{ "$ref": "#/$defs/Missing" }"##),
                )
            },
            &["mnemonic.noteSelection.schema"],
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
                    json(r#""note.pin""#),
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
            "combined ref",
            |d| {
                set(
                    d,
                    "/$defs/NoteSummary/properties/title",
                    json(r##"{ "$ref": "#/$defs/SortKey", "description": "x" }"##),
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
            "/$defs/NoteSummary/properties/sort",
            r#"{ "type": ["string", "integer"] }"#,
            "NoteSummary.sort",
        ),
        ("/$defs/Label", r#"{ "type": "string" }"#, "$defs.Label"),
    ];
    for (pointer, schema, fragment) in cases {
        let mut document = notebook_document();
        set(&mut document, pointer, json(schema));
        let definition = Definition::from_json(document, &config.project)
            .unwrap_or_else(|error| panic!("{pointer} validates: {error}"));
        match build_outputs(&definition, &config) {
            Err(Error::Definition(message)) => assert_mentions(&message, &[fragment]),
            other => panic!("{pointer}: expected a refusal, got {other:?}"),
        }
    }
}

/// Ownership gaps validation admits are refused by the coverage report.
#[test]
fn incomplete_ownership_is_refused_at_generation() {
    let config = notebook_config();
    let render = |document: Json| {
        let definition = Definition::from_json(document, &config.project).expect("validates");
        build_outputs(&definition, &config)
    };

    let mut unclassified = notebook_document();
    let archive = unclassified
        .pointer("/mutations/task.archive")
        .cloned()
        .expect("declared");
    set(&mut unclassified, "/mutations/task.touch", archive);
    match render(unclassified) {
        Err(Error::Definition(message)) => {
            assert_mentions(&message, &["entity:Task:authoringMutationClassification"])
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    let mut shared_key = notebook_document();
    let key = shared_key
        .pointer("/mnemonic/board.view/key")
        .cloned()
        .expect("declared");
    set(&mut shared_key, "/mnemonic/noteSelection/key", key);
    match render(shared_key) {
        Err(Error::Definition(message)) => {
            assert_mentions(&message, &["board.view", "noteSelection"])
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
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

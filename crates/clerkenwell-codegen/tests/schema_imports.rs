//! Schema imports: a definition imports the definitions it reaches from the
//! bundles its configuration names, each converted to the subset's form of
//! the same values or refused where the subset has none.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clerkenwell_codegen::{build_outputs, Config, Definition, Error, GeneratedOutputs};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A definition, written in `directory`, that is the notebook's with `probe`
/// added to its `$defs`, so the probe's references reach an imported bundle.
fn probe_definition(directory: &Path, probe: Value) -> PathBuf {
    let mut document: Value =
        serde_json::from_str(&common::notebook_document().stringify()).unwrap();
    document["$defs"]["ImportProbe"] = probe;
    let path = directory.join("definition.json");
    fs::write(&path, document.to_string()).unwrap();
    path
}

/// A probe object with one optional member per name, each referencing it.
fn referencing(names: &[&str]) -> Value {
    let properties: serde_json::Map<String, Value> = names
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                json!({"$ref": format!("#/$defs/{name}")}),
            )
        })
        .collect();
    json!({"type": "object", "additionalProperties": false, "properties": properties})
}

/// Imports `bundle` into the notebook definition beside a probe referencing
/// `names`.
fn import(bundle: Value, names: &[&str]) -> Result<Definition, Error> {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("shared.schema.json");
    fs::write(&schema, bundle.to_string()).unwrap();
    let definition = probe_definition(directory.path(), referencing(names));
    Definition::load_with_schemas(&definition, &[schema])
}

fn outputs(definition: &Definition) -> GeneratedOutputs {
    build_outputs(definition, &common::notebook_config()).expect("the definition generates")
}

/// The definition `name`, as the normalised definition holds it.
fn normalised(outputs: &GeneratedOutputs, name: &str) -> Value {
    let normalised: Value = serde_json::from_str(&outputs.normalized_definition).unwrap();
    normalised["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|definition| definition["name"] == name)
        .unwrap_or_else(|| panic!("{name} is a definition"))
        .clone()
}

fn refusal(result: Result<Definition, Error>) -> String {
    match result {
        Err(Error::Definition(message)) => message,
        Err(other) => panic!("expected a definition refusal, got {other}"),
        Ok(_) => panic!("expected the import to be refused"),
    }
}

#[test]
fn only_the_definitions_a_definition_reaches_are_imported() {
    let bundle = json!({"$defs": {
        "Reached": {"type": "object", "additionalProperties": false, "properties": {
            "next": {"$ref": "#/$defs/ReachedThrough"}
        }},
        "ReachedThrough": {"type": "string"},
        "Unreached": {"type": "string", "pattern": "[a-z]+"},
        "AlsoUnreached": {"type": "integer", "format": "uint128"}
    }});
    let definition =
        import(bundle, &["Reached"]).expect("the unreached definitions are not imported");
    let outputs = outputs(&definition);
    for name in ["Reached", "ReachedThrough"] {
        assert!(
            outputs
                .typescript_model
                .contains(&format!("export type {name} =")),
            "{name}"
        );
    }
    for name in ["Unreached", "AlsoUnreached"] {
        assert!(!outputs.typescript_model.contains(name), "{name}");
        assert!(!outputs.rust_model.contains(name), "{name}");
    }
}

#[test]
fn a_reached_definition_the_subset_cannot_represent_is_refused_by_name() {
    let message = refusal(import(
        json!({"$defs": {"Pattern": {"type": "string", "pattern": "[a-z]+"}}}),
        &["Pattern"],
    ));
    assert!(message.contains("$defs.Pattern"), "{message}");
    assert!(message.contains("pattern"), "{message}");
}

#[test]
fn imports_refuse_duplicate_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("shared.schema.json");
    fs::write(
        &schema,
        json!({"$defs": {"Shared": {"type": "string"}}}).to_string(),
    )
    .unwrap();
    let definition = probe_definition(directory.path(), referencing(&["Shared"]));
    assert!(Definition::load_with_schemas(&definition, &[schema.clone(), schema]).is_err());
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("shared.schema.json");
    fs::write(
        &schema,
        json!({"$defs": {"NoteSummary": {"type": "string"}}}).to_string(),
    )
    .unwrap();
    let definition = probe_definition(directory.path(), referencing(&["NoteSummary"]));
    assert!(Definition::load_with_schemas(&definition, &[schema]).is_err());
}

#[test]
fn imported_paths_resolve_against_configuration_and_reads_leave_sources_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codegen.json");
    let source = json!({"definition": "definition.json", "schemaImports": ["assets/shared.schema.json"], "regenerateCommand": "generate", "outputs": {
        "collaborationFixtures": "collaboration.json", "contractFixtures": "contract.json", "normalizedDefinition": "ir.json", "typescriptModel": "model.ts", "rustModel": "model.rs"
    }}).to_string();
    fs::write(&path, &source).unwrap();
    let config = Config::load(&path).unwrap();
    assert_eq!(
        config.schema_imports,
        vec![directory.path().join("assets/shared.schema.json")]
    );
    assert_eq!(fs::read_to_string(path).unwrap(), source);
}

#[test]
fn recursive_records_string_enums_and_tagged_unions_import() {
    let definition = import(
        json!({"$defs": {
            "SharedMode": {"type": "string", "enum": ["compact", "expanded"]},
            "SharedRecord": {"type": "object", "additionalProperties": false, "required": ["revision", "mode", "extensions"], "properties": {
                "revision": {"type": "integer", "minimum": 0},
                "mode": {"$ref": "#/$defs/SharedMode"},
                "parent": {"$ref": "#/$defs/SharedRecord"},
                "extensions": {"type": "object", "additionalProperties": true}
            }},
            "SharedChange": {"oneOf": [
                {"type": "object", "additionalProperties": false, "required": ["operation", "record"], "properties": {"operation": {"const": "insert"}, "record": {"$ref": "#/$defs/SharedRecord"}}},
                {"type": "object", "additionalProperties": false, "required": ["operation"], "properties": {"operation": {"const": "remove"}}}
            ]}
        }}),
        &["SharedChange"],
    )
    .unwrap();
    let outputs = outputs(&definition);
    assert!(outputs.rust_model.contains("Option<Box<SharedRecord>>"));
    assert!(outputs.rust_model.contains("pub enum SharedMode"));
    assert!(outputs
        .rust_model
        .contains("#[serde(tag = \"operation\", deny_unknown_fields)]"));
    assert_eq!(
        normalised(&outputs, "SharedRecord")["properties"]["revision"]["minimum"],
        0
    );
}

#[test]
fn reference_annotations_preserve_defaults_and_validate_the_referenced_type() {
    let mut bundle = json!({"$defs": {
        "Choice": {"type": "string", "enum": ["first", "second"]},
        "Selection": {"type": "object", "additionalProperties": false, "properties": {
            "choice": {"$ref": "#/$defs/Choice", "default": "second", "description": "Selection"}
        }}
    }});
    assert!(import(bundle.clone(), &["Selection"]).is_ok());
    bundle["$defs"]["Selection"]["properties"]["choice"]["default"] = json!(false);
    assert!(import(bundle, &["Selection"]).is_err());
}

#[test]
fn a_nullable_value_imports_as_the_value_or_null() {
    let definition = import(
        json!({"$defs": {
            "Target": {"type": "object", "additionalProperties": false, "properties": {}},
            "Holder": {"type": "object", "additionalProperties": false, "required": ["required_text", "required_target"], "properties": {
                "required_text": {"type": ["string", "null"]},
                "optional_text": {"type": ["string", "null"]},
                "required_target": {"anyOf": [{"$ref": "#/$defs/Target"}, {"type": "null"}]},
                "optional_target": {"oneOf": [{"type": "null"}, {"$ref": "#/$defs/Target"}]},
                "optional_items": {"type": ["array", "null"], "items": {"type": "string"}}
            }}
        }}),
        &["Holder"],
    )
    .unwrap();
    let outputs = outputs(&definition);
    for typescript in [
        "\"required_text\": string | null;",
        "\"optional_text\"?: string | null;",
        "\"required_target\": Target | null;",
        "\"optional_target\"?: Target | null;",
        "\"optional_items\"?: string[] | null;",
    ] {
        assert!(
            outputs.typescript_model.contains(typescript),
            "{typescript}"
        );
    }
    for rust in [
        "pub required_text: Option<String>,",
        "pub optional_text: Option<Option<String>>,",
        "pub required_target: Option<Target>,",
        "pub optional_target: Option<Option<Target>>,",
        "pub optional_items: Option<Option<Vec<String>>>,",
    ] {
        assert!(outputs.rust_model.contains(rust), "{rust}");
    }
    assert!(outputs
        .rust_model
        .contains("#[serde(deserialize_with = \"clerkenwell_schema::present\")]"));
}

#[test]
fn an_any_of_other_than_a_value_or_null_is_refused() {
    let message = refusal(import(
        json!({"$defs": {"Either": {"anyOf": [{"type": "string"}, {"type": "integer"}]}}}),
        &["Either"],
    ));
    assert!(message.contains("one schema paired with null"), "{message}");
}

#[test]
fn an_enum_written_as_described_alternatives_keeps_each_description() {
    let description = "It finished.";
    let definition = import(
        json!({"$defs": {"Outcome": {"description": "What happened.", "oneOf": [
            {"type": "string", "const": "done", "description": description},
            {"type": "string", "enum": ["skipped", "failed"]}
        ]}}}),
        &["Outcome"],
    )
    .unwrap();
    let outputs = outputs(&definition);
    let outcome = normalised(&outputs, "Outcome");
    assert_eq!(outcome["enum"], json!(["done", "skipped", "failed"]));
    assert_eq!(outcome["enumDescriptions"]["done"], description);
    assert!(outputs.rust_model.contains(&format!(
        "    /// {description}\n    #[serde(rename = \"done\")]"
    )));
    assert!(outputs
        .typescript_model
        .contains("export type Outcome = \"done\" | \"skipped\" | \"failed\";"));
}

#[test]
fn numeric_formats_bound_integers_to_the_models_64_bit_integers() {
    let definition = import(
        json!({"$defs": {"Numbers": {"type": "object", "additionalProperties": false, "properties": {
            "small": {"type": "integer", "format": "uint32", "minimum": 0.5, "maximum": 3.5},
            "unsigned": {"type": "integer", "format": "uint64", "minimum": 0},
            "pointer": {"type": "integer", "format": "uint"},
            "signed": {"type": "integer", "format": "int64"},
            "nullable": {"type": ["integer", "null"], "format": "uint16"},
            "single": {"type": "number", "format": "float"},
            "double": {"type": "number", "format": "double"},
            "positive": {"type": "integer", "exclusiveMinimum": 0},
            "stamp": {"type": "string", "format": "date-time"}
        }}}}),
        &["Numbers"],
    )
    .unwrap();
    let numbers = normalised(&outputs(&definition), "Numbers");
    let property = |name: &str| numbers["properties"][name].clone();
    assert_eq!(property("small")["minimum"], 0.5);
    assert_eq!(property("small")["maximum"], 3.5);
    for name in ["unsigned", "pointer"] {
        assert_eq!(property(name)["minimum"], 0, "{name}");
        assert!(property(name).get("maximum").is_none(), "{name}");
    }
    assert!(property("signed").get("minimum").is_none());
    assert_eq!(property("nullable")["maximum"], 65_535);
    assert_eq!(property("single")["maximum"], f32::MAX as f64);
    assert!(property("double").get("maximum").is_none());
    assert_eq!(property("positive")["minimum"], 1);
    assert_eq!(property("stamp"), json!({"type": "string"}));
    for (format, kind) in [
        ("uint128", "integer"),
        ("int128", "integer"),
        ("int64", "number"),
    ] {
        assert!(
            import(
                json!({"$defs": {"Wide": {"type": kind, "format": format}}}),
                &["Wide"]
            )
            .is_err(),
            "{format}"
        );
    }
    assert!(import(
        json!({"$defs": {"Exclusive": {"type": "number", "exclusiveMaximum": 1.5}}}),
        &["Exclusive"]
    )
    .is_err());
}

#[test]
fn an_object_that_does_not_close_its_members_admits_any_other_member() {
    let definition = import(
        json!({"$defs": {
            "Declared": {"type": "object", "required": ["kind"], "properties": {"kind": {"type": "string"}}},
            "Anything": {"type": "object"}
        }}),
        &["Declared", "Anything"],
    )
    .unwrap();
    let outputs = outputs(&definition);
    assert!(outputs
        .rust_model
        .contains("pub other_members: OtherMembers<Declared, JsonValue>,"));
    assert!(outputs.typescript_model.contains(
        "export type Declared = {\n  \"kind\": string;\n  [member: string]: unknown | undefined;\n};"
    ));
    assert!(outputs
        .typescript_model
        .contains("export type Anything = Record<string, unknown>;"));
}

#[test]
fn annotations_without_constraints_are_dropped_and_constants_imply_their_type() {
    let definition = import(
        json!({"$defs": {"Annotated": {"type": "object", "additionalProperties": false, "properties": {
            "legacy": {"type": "string", "deprecated": true, "examples": ["old"], "readOnly": true},
            "version": {"const": 2}
        }}}}),
        &["Annotated"],
    )
    .unwrap();
    let annotated = normalised(&outputs(&definition), "Annotated");
    assert_eq!(annotated["properties"]["legacy"], json!({"type": "string"}));
    assert_eq!(annotated["properties"]["version"]["type"], "integer");
}

#[test]
fn an_untagged_union_is_refused() {
    let message = refusal(import(
        json!({"$defs": {"Untagged": {"oneOf": [{"type": "string"}, {"type": "integer"}]}}}),
        &["Untagged"],
    ));
    assert!(message.contains("$defs.Untagged"), "{message}");
}

// A program's serde types, and the bundle `schemars` writes for them, as a
// consumer of Clerkenwell receives it from another project.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    /// The call was never sent.
    NotSent,
    /// The call completed.
    Completed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
enum Detail<T> {
    Absent,
    Present(T),
    Withheld,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Exchange {
    attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Cause {
    Failure(Failure),
    Foreign(Detail<Vec<String>>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
struct Failure(Box<FailureParts>);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct FailureParts {
    message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    context: BTreeMap<String, Value>,
    exchange: Exchange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cause: Option<Cause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cleanup: Option<Failure>,
    native: Detail<Value>,
    ratio: f32,
    count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event {
    Started { id: String },
    Stopped { id: String, reason: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Mixed {
    Idle,
    Count(i64),
    Pair { left: String, right: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Label(String);

/// A type the subset has no form of, which the bundle declares and no
/// definition reaches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Unreached {
    huge: u128,
}

/// The probe projection's snapshot, as the program reading it declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct View {
    failure: Failure,
    event: Event,
    mixed: Mixed,
    label: Label,
}

/// The bundle `schemars` writes for the program's types, and the schema it
/// writes for a use of each of the view's members.
fn program_bundle() -> (Value, Value) {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let members = json!({
        "failure": generator.subschema_for::<Failure>(),
        "event": generator.subschema_for::<Event>(),
        "mixed": generator.subschema_for::<Mixed>(),
        "label": generator.subschema_for::<Label>(),
    });
    generator.subschema_for::<Unreached>();
    (json!({"$defs": generator.take_definitions(true)}), members)
}

/// The notebook definition with a projection whose snapshot holds the
/// program's types, importing the program's bundle.
fn program_definition() -> (tempfile::TempDir, Definition) {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("program.schema.json");
    let (bundle, members) = program_bundle();
    fs::write(&schema, bundle.to_string()).unwrap();
    let mut document: Value =
        serde_json::from_str(&common::notebook_document().stringify()).unwrap();
    document["$defs"]["ProgramView"] = json!({"type": "object", "additionalProperties": false,
        "required": ["failure", "event", "mixed", "label"], "properties": members});
    document["$defs"]["ProgramViewPatch"] = json!({"type": "object", "additionalProperties": false,
    "required": ["kind"], "properties": {
        "kind": {"type": "string", "enum": ["replace"]},
        "view": {"$ref": "#/$defs/ProgramView"}
    }});
    document["projections"]["probe.view"] = json!({
        "description": "The program's types, as one view.",
        "params": {"$ref": "#/$defs/WorkspaceParams"},
        "snapshot": {"$ref": "#/$defs/ProgramView"},
        "patch": {"$ref": "#/$defs/ProgramViewPatch"},
        "dependsOn": ["SyncHealth"],
        "materialization": {"strategy": "replace", "snapshotMode": "field", "snapshotField": "view"}
    });
    let path = directory.path().join("definition.json");
    fs::write(&path, document.to_string()).unwrap();
    let definition = Definition::load_with_schemas(&path, &[schema])
        .unwrap_or_else(|error| panic!("the program's bundle imports: {error}"));
    (directory, definition)
}

/// The JSON Schema the generated Rust model states in `constant`.
fn schema_constant(rust_model: &str, constant: &str) -> Value {
    let start = rust_model
        .find(&format!("pub const {constant}: &str ="))
        .unwrap_or_else(|| panic!("the model declares {constant}"));
    let rest = &rust_model[start..];
    let open = rest.find("r#").expect("a raw string");
    let hashes = rest[open + 1..].chars().take_while(|c| *c == '#').count();
    let body_start = open + 1 + hashes + 1;
    let close = format!("\"{}", "#".repeat(hashes));
    let body_end = body_start
        + rest[body_start..]
            .find(&close)
            .expect("a closed raw string");
    serde_json::from_str(&rest[body_start..body_end]).expect("the constant is JSON")
}

/// Views holding one value of each variant the program writes.
fn program_values() -> Vec<View> {
    let parts = |cause: Option<Cause>, native: Detail<Value>| FailureParts {
        message: "fixed".into(),
        context: BTreeMap::from([("node".into(), json!("n1"))]),
        exchange: Exchange {
            attempts: 2,
            retry_after_ms: Some(1500),
            request_id: None,
            outcome: Outcome::Completed,
        },
        cause,
        cleanup: None,
        native,
        ratio: 0.5,
        count: 3,
    };
    let inner = Failure(Box::new(parts(None, Detail::Withheld)));
    let failures = [
        Failure(Box::new(parts(None, Detail::Absent))),
        Failure(Box::new(parts(
            Some(Cause::Failure(inner)),
            Detail::Present(json!({"error": {"message": "native"}})),
        ))),
        Failure(Box::new(parts(
            Some(Cause::Foreign(Detail::Present(vec!["lower".into()]))),
            Detail::Present(Value::Null),
        ))),
    ];
    let events = [
        Event::Started { id: "e1".into() },
        Event::Stopped {
            id: "e1".into(),
            reason: Some("done".into()),
        },
        Event::Stopped {
            id: "e1".into(),
            reason: None,
        },
    ];
    let mixed = [
        Mixed::Idle,
        Mixed::Count(-4),
        Mixed::Pair {
            left: "l".into(),
            right: "r".into(),
        },
    ];
    failures
        .into_iter()
        .zip(events)
        .zip(mixed)
        .map(|((failure, event), mixed)| View {
            failure,
            event,
            mixed,
            label: Label("label".into()),
        })
        .collect()
}

#[test]
fn a_bundle_schemars_writes_for_serde_types_imports_without_what_it_does_not_reach() {
    let (_directory, definition) = program_definition();
    let outputs = outputs(&definition);
    for name in ["FailureParts", "Cause", "Event", "Mixed", "Label"] {
        assert!(
            outputs
                .typescript_model
                .contains(&format!("export type {name} =")),
            "{name}"
        );
    }
    assert!(!outputs.rust_model.contains("Unreached"));
    assert!(!outputs.typescript_model.contains("Unreached"));
}

#[test]
fn the_imported_types_fixture_is_a_value_the_program_reads() {
    let (_directory, definition) = program_definition();
    let fixtures: Value = serde_json::from_str(&outputs(&definition).contract_fixtures).unwrap();
    let snapshot = fixtures["projections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fixture| fixture["projection"] == "probe.view")
        .expect("a fixture of the probe projection")["snapshot"]["value"]
        .clone();
    // The program reads the members its types declare; serde ignores the
    // others, which the program's schema admits.
    if let Err(error) = serde_json::from_value::<View>(snapshot.clone()) {
        panic!("{snapshot} is not a value of the program's types: {error}");
    }
}

#[test]
fn every_value_the_program_writes_meets_the_imported_schema() {
    let (_directory, definition) = program_definition();
    let schema = schema_constant(
        &outputs(&definition).rust_model,
        "PROBE_VIEW_PROJECTION_SNAPSHOT_SCHEMA",
    );
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    for view in program_values() {
        let value = serde_json::to_value(&view).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{value}: {errors:?}");
    }
    let failure = serde_json::to_value(&program_values()[0].failure).unwrap();
    for refused in [
        json!({"failure": "not a failure", "event": {"kind": "started", "id": "e"}, "mixed": "idle", "label": "l"}),
        json!({"failure": failure, "event": {"kind": "paused", "id": "e"}, "mixed": "idle", "label": "l"}),
        json!({"failure": failure, "event": {"kind": "started", "id": "e"}, "mixed": {"count": "four"}, "label": "l"}),
    ] {
        assert!(!validator.is_valid(&refused), "{refused}");
    }
}

#[test]
fn the_imported_types_normalise_as_the_subset_spells_them() {
    let (_directory, definition) = program_definition();
    let outputs = outputs(&definition);
    let outcome = normalised(&outputs, "Outcome");
    assert_eq!(outcome["enum"], json!(["not_sent", "completed"]));
    assert_eq!(
        outcome["enumDescriptions"]["not_sent"],
        "The call was never sent."
    );
    let cause = normalised(&outputs, "Cause");
    assert!(cause.get("discriminator").is_none());
    assert!(cause["oneOf"].get("failure").is_some());
    let exchange = normalised(&outputs, "Exchange");
    assert_eq!(exchange["properties"]["retry_after_ms"]["minimum"], 0);
    let mixed = normalised(&outputs, "Mixed");
    assert_eq!(mixed["enum"], json!(["idle"]));
    assert!(mixed["oneOf"].get("pair").is_some());
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Tags {
    tags: std::collections::BTreeSet<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
enum Either {
    Text(String),
    Count(i64),
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Point {
    at: (i64, i64),
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Huge {
    value: u128,
}

/// The bundle `schemars` writes for `T`, imported by a probe referencing it.
fn import_type<T: JsonSchema>() -> Result<Definition, Error> {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let reference = generator.subschema_for::<T>();
    let name = reference
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .expect("a named type")
        .to_owned();
    import(json!({"$defs": generator.take_definitions(true)}), &[&name])
}

/// The forms the subset holds no representation of, as `schemars` writes them
/// for serde types, are refused naming the definition.
#[test]
fn the_serde_types_the_subset_cannot_represent_are_refused() {
    for (name, result) in [
        ("Tags", import_type::<Tags>()),
        ("Either", import_type::<Either>()),
        ("Point", import_type::<Point>()),
        ("Huge", import_type::<Huge>()),
    ] {
        let message = refusal(result);
        assert!(
            message.contains(&format!("$defs.{name}")),
            "{name}: {message}"
        );
    }
}

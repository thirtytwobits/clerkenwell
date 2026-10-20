//! Generation is a pure function of the definition and configuration, `--check`
//! reports exactly the artefacts that differ from it, and a write converges.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use clerkenwell_codegen::testing::{self, normalized_document, Json, Object};
use clerkenwell_codegen::{build_outputs, generate, Config, Definition, Error, Mode};
use common::{
    copy_notebook, json, notebook_config, notebook_config_path, notebook_document, push, set,
    validate, with_defs,
};

fn reversed(object: &Object) -> Object {
    let mut entries: Vec<(&str, &Json)> = object.iter().collect();
    entries.reverse();
    entries
        .into_iter()
        .map(|(key, value)| (key, value.clone()))
        .collect()
}

fn reverse_section(document: &mut Json, pointer: &str) {
    let section = document
        .pointer(pointer)
        .and_then(Json::as_object)
        .unwrap_or_else(|| panic!("{pointer} is an object"));
    let flipped = reversed(section);
    set(document, pointer, flipped.into());
}

#[test]
fn normalised_ir_is_deterministic_across_source_object_ordering() {
    let mut shuffled = notebook_document();
    for pointer in [
        "/entities",
        "/projections",
        "/mutations",
        "/collaboration/entities",
        "/collaboration/entities/Board/fields",
        "/$defs",
    ] {
        reverse_section(&mut shuffled, pointer);
    }
    assert_ne!(
        shuffled.stringify(),
        notebook_document().stringify(),
        "the reordering must change the source"
    );
    let original = validate(notebook_document()).expect("valid");
    let shuffled = validate(shuffled).expect("reordering keeps it valid");
    assert_eq!(
        normalized_document(&shuffled).stringify_pretty(),
        normalized_document(&original).stringify_pretty()
    );
}

#[test]
fn generated_outputs_are_deterministic() {
    let config = notebook_config();
    let definition = Definition::load(&config.definition).expect("valid");
    let first = build_outputs(&definition, &config).expect("renders");
    let second = build_outputs(&definition, &config).expect("renders");
    assert_eq!(first, second);
    for (path, content) in first.with_paths(&config.outputs) {
        let committed = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("{} is committed: {error}", path.display()));
        assert!(committed == content, "{} is stale", path.display());
    }
}

#[test]
fn the_committed_example_outputs_pass_check() {
    let changed = generate(&notebook_config(), Mode::Check).expect("the example is current");
    assert!(changed.is_empty(), "{changed:?}");
}

#[test]
fn generated_rust_satisfies_rustfmt() {
    let config = notebook_config();
    let definition = Definition::load(&config.definition).expect("valid");
    assert_rustfmt_clean(&definition, &config);
}

/// An open object's declared member names wrap as rustfmt wraps them inside
/// the `impl` that lists them.
#[test]
fn an_open_object_with_many_declared_members_satisfies_rustfmt() {
    let short = [
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
    ];
    let long = short.map(|name| format!("declared_member_{name}"));
    let long: Vec<&str> = long.iter().map(String::as_str).collect();
    for names in [&short[..], &long[..]] {
        for count in 1..=names.len() {
            assert_open_object_rustfmt_clean(&names[..count], r#"{ "type": "string" }"#);
        }
    }
}

/// An open object's other-member constraints are laid out as rustfmt lays
/// them out.
#[test]
fn an_open_object_with_constrained_other_members_satisfies_rustfmt() {
    for other_members in [
        r#"{ "type": "string", "minLength": 2 }"#,
        r#"{ "type": "string", "minLength": 2, "const": "a-rather-long-constant-value" }"#,
        r#"{ "type": "array", "items": { "type": "string" }, "default": ["a", "b"] }"#,
        r#"{ "type": "array", "items": { "type": "array",
            "items": { "type": "string", "minLength": 2 } } }"#,
    ] {
        assert_open_object_rustfmt_clean(&[], other_members);
    }
}

/// `names` declared as members beside `other_members`; with no names, one
/// member of the other members' own schema.
fn assert_open_object_rustfmt_clean(names: &[&str], other_members: &str) {
    let mut properties: Vec<String> = names
        .iter()
        .map(|name| format!(r#""{name}": {{ "type": "string" }}"#))
        .collect();
    if properties.is_empty() {
        properties.push(format!(r#""member": {other_members}"#));
    }
    let document = with_defs(&format!(
        r#"{{ "ProbeOpen": {{ "type": "object", "properties": {{ {} }},
            "additionalProperties": {other_members} }} }}"#,
        properties.join(", ")
    ));
    let definition = testing::definition(document).expect("valid");
    assert_rustfmt_clean(&definition, &notebook_config());
}

fn assert_rustfmt_clean(definition: &Definition, config: &Config) {
    let outputs = build_outputs(definition, config).expect("renders");
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("model.rs");
    std::fs::write(&path, &outputs.rust_model).expect("written");
    let status = Command::new("rustfmt")
        .args(["--check", "--edition", "2021"])
        .arg(&path)
        .status()
        .expect("rustfmt runs");
    assert!(status.success(), "the generated Rust is not rustfmt-clean");
}

/// The generated Rust imports `JsonValue` and `BTreeMap` exactly when its types
/// use them: the notebook uses both, the notes example neither.
#[test]
fn generated_rust_imports_only_what_it_uses() {
    let notes =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/notes/clerkenwell-codegen.json");
    let configs = [
        notebook_config(),
        Config::load(&notes).expect("the notes configuration loads"),
    ];
    for config in configs {
        let definition = Definition::load(&config.definition).expect("valid");
        let rust = build_outputs(&definition, &config)
            .expect("renders")
            .rust_model;
        for (import, name) in [
            ("use serde_json::Value as JsonValue;", "JsonValue"),
            ("use std::collections::BTreeMap;", "BTreeMap"),
        ] {
            let imported = rust.lines().any(|line| line == import);
            let used = rust
                .lines()
                .any(|line| line != import && line.contains(name));
            assert_eq!(imported, used, "{}: {import}", config.definition.display());
        }
    }
}

#[test]
fn every_string_enum_a_tagged_union_variant_declares_is_emitted_under_its_variant() {
    let mut document = notebook_document();
    for (variant, values) in [
        ("blocked", r#"["minor", "major"]"#),
        ("done", r#"["checked", "waived"]"#),
    ] {
        set(
            &mut document,
            &format!("/$defs/TaskState/oneOf/{variant}/properties/level"),
            json(&format!(r#"{{ "type": "string", "enum": {values} }}"#)),
        );
    }
    let definition = validate(document).expect("valid");

    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;

    for (variant, first, second) in [("Blocked", "Minor", "Major"), ("Done", "Checked", "Waived")] {
        let name = format!("TaskState{variant}Level");
        assert!(
            rust.contains(&format!("pub enum {name} {{")),
            "{name} is emitted:\n{rust}"
        );
        let declared = rust
            .split(&format!("pub enum {name} {{"))
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("its variants");
        assert!(
            declared.contains(first) && declared.contains(second),
            "{declared}"
        );
        assert!(
            rust.contains(&format!("level: Option<{name}>")),
            "the {variant} variant's field is {name}"
        );
    }
}

/// The generated TypeScript of `document`.
fn typescript_of(document: Json) -> String {
    let definition = validate(document).expect("valid");
    build_outputs(&definition, &notebook_config())
        .expect("renders")
        .typescript_model
}

/// Each line of the generated `CollaborationDocument` map, by entity.
fn collaboration_documents(typescript: &str) -> Vec<(String, String)> {
    typescript
        .split("export type CollaborationDocument<TEntity extends CollaborationEntityName> = {\n")
        .nth(1)
        .and_then(|rest| rest.split("}[TEntity];").next())
        .expect("the collaboration document map")
        .lines()
        .map(|line| {
            let (entity, held) = line.trim().split_once(": ").expect("an entity's line");
            (entity.trim_matches('"').to_owned(), held.to_owned())
        })
        .collect()
}

/// The paths at which a replica may leave out a member of `entity`'s
/// document, as the generated `CollaborationDocument` names them.
fn replica_optional_paths(typescript: &str, entity: &str) -> Vec<String> {
    let held = collaboration_documents(typescript)
        .into_iter()
        .find(|(name, _)| name == entity)
        .unwrap_or_else(|| panic!("{entity} is a collaborative entity"))
        .1;
    let arguments = held
        .strip_prefix("CollaborationReplicaDocument<")
        .and_then(|rest| rest.strip_suffix(">;"))
        .unwrap_or_else(|| panic!("{entity}: {held}"));
    arguments
        .split_once(", ")
        .map(|(_, paths)| {
            paths
                .split(" | ")
                .map(|path| path.trim_matches('"').to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn each_collaborative_entity_holds_its_schema_document_type() {
    let document = notebook_document();
    let typescript = typescript_of(document.clone());
    let collaborative = document
        .pointer("/collaboration/entities")
        .and_then(Json::as_object)
        .expect("collaborative entities");
    let documents = collaboration_documents(&typescript);

    assert!(!collaborative.is_empty());
    assert_eq!(
        documents
            .iter()
            .map(|(entity, _)| entity.as_str())
            .collect::<Vec<_>>(),
        collaborative.keys().collect::<Vec<_>>()
    );
    for (entity, held) in &documents {
        let schema = document
            .pointer(&format!("/entities/{entity}/schema/$ref"))
            .and_then(Json::as_str)
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .expect("an entity schema");
        let opened = format!("CollaborationReplicaDocument<{schema}");
        assert!(
            held.strip_prefix(&opened)
                .is_some_and(|rest| rest.starts_with(", ") || rest.starts_with('>')),
            "{entity}: {held}"
        );
    }
}

#[test]
fn a_replica_may_leave_out_a_root_identity_and_a_revision_but_holds_each_item_identity() {
    let typescript = typescript_of(notebook_document());

    for (entity, identity) in [("Note", "note_id"), ("Board", "board_id")] {
        let paths = replica_optional_paths(&typescript, entity);
        assert!(
            paths.iter().any(|path| path == identity),
            "{entity}: {paths:?}"
        );
        assert!(
            paths.iter().any(|path| path == "etag"),
            "{entity}: {paths:?}"
        );
    }
    let board = replica_optional_paths(&typescript, "Board");
    for identity in ["columns.*.column_id", "columns.*.cards.*.card_id"] {
        assert!(!board.iter().any(|path| path == identity), "{board:?}");
    }
}

#[test]
fn a_replica_may_leave_out_only_members_the_schema_requires() {
    let typescript = typescript_of(notebook_document());
    let note = replica_optional_paths(&typescript, "Note");

    for optional in ["source", "cover_image", "summary"] {
        assert!(!note.iter().any(|path| path == optional), "{note:?}");
    }
}

/// A required object no field lies beneath is never written, so a replica's
/// document may lack it.
#[test]
fn a_replica_may_leave_out_a_required_object_no_field_lies_beneath() {
    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/NoteMarker",
        json(r#"{ "type": "object", "additionalProperties": false, "properties": {} }"#),
    );
    set(
        &mut document,
        "/$defs/NoteDocument/properties/marker",
        json(r##"{ "$ref": "#/$defs/NoteMarker" }"##),
    );
    push(
        &mut document,
        "/$defs/NoteDocument/required",
        json(r#""marker""#),
    );
    validate(document.clone()).expect("a closed empty object needs no field");

    let left_out = replica_optional_paths(&typescript_of(document), "Note");

    assert!(
        left_out.iter().any(|path| path == "marker"),
        "no field lies beneath marker: {left_out:?}"
    );
}

/// A replica writes an object only beneath a field it writes, so it may leave
/// out an object the schema requires unless a field beneath it is in every
/// document.
#[test]
fn a_replica_may_leave_out_a_required_object_unless_a_field_beneath_it_always_is() {
    let kept = replica_optional_paths(&typescript_of(notebook_document()), "Note");
    assert!(
        !kept.iter().any(|path| path == "meta"),
        "meta.owner is in every document: {kept:?}"
    );

    let mut document = notebook_document();
    set(&mut document, "/$defs/NoteMeta/required", json("[]"));
    set(
        &mut document,
        "/collaboration/entities/Note/fields/meta.owner/required",
        json("false"),
    );
    let typescript = typescript_of(document);
    let left_out = replica_optional_paths(&typescript, "Note");
    assert!(
        left_out.iter().any(|path| path == "meta"),
        "no field beneath meta is in every document: {left_out:?}"
    );
    let wire = typescript
        .split("export type NoteDocument = {")
        .nth(1)
        .and_then(|rest| rest.split("\n};").next())
        .expect("NoteDocument is declared");
    assert!(
        wire.lines()
            .any(|line| line.trim().starts_with("\"meta\":")),
        "the document type still requires meta: {wire}"
    );
}

/// A configuration pointing every output into `directory`, over the example
/// definition.
fn config_writing_into(directory: &Path) -> Config {
    let mut config = notebook_config();
    let outputs = &mut config.outputs;
    for path in [
        &mut outputs.collaboration_fixtures,
        &mut outputs.contract_fixtures,
        &mut outputs.normalized_definition,
        &mut outputs.typescript_model,
        &mut outputs.rust_model,
    ] {
        let relative = path
            .strip_prefix(common::notebook_dir())
            .expect("the example's outputs sit beside it")
            .to_owned();
        *path = directory.join(relative);
    }
    config
}

fn all_output_paths(config: &Config) -> Vec<PathBuf> {
    let outputs = &config.outputs;
    vec![
        outputs.collaboration_fixtures.clone(),
        outputs.contract_fixtures.clone(),
        outputs.normalized_definition.clone(),
        outputs.typescript_model.clone(),
        outputs.rust_model.clone(),
    ]
}

fn stale_paths(config: &Config) -> Vec<PathBuf> {
    match generate(config, Mode::Check) {
        Err(Error::Stale { paths, .. }) => paths,
        Ok(_) => Vec::new(),
        Err(other) => panic!("expected a stale report, got {other}"),
    }
}

#[test]
fn check_writes_nothing_and_lists_every_missing_output() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let config = config_writing_into(directory.path());
    let mut stale = stale_paths(&config);
    let mut expected = all_output_paths(&config);
    stale.sort();
    expected.sort();
    assert_eq!(stale, expected);
    assert!(
        std::fs::read_dir(directory.path())
            .expect("readable")
            .next()
            .is_none(),
        "check wrote into the output directory"
    );
}

#[test]
fn check_fails_when_generated_outputs_drift() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let config = config_writing_into(directory.path());
    generate(&config, Mode::Write).expect("writes");
    std::fs::write(&config.outputs.typescript_model, "stale\n").expect("overwritten");
    std::fs::remove_file(&config.outputs.normalized_definition).expect("removed");

    let error = generate(&config, Mode::Check).expect_err("drift is stale");
    let Error::Stale { command, paths } = &error else {
        panic!("expected a stale report, got {error}");
    };
    let mut paths = paths.clone();
    paths.sort();
    let mut expected = vec![
        config.outputs.typescript_model.clone(),
        config.outputs.normalized_definition.clone(),
    ];
    expected.sort();
    assert_eq!(paths, expected);
    let message = error.to_string();
    assert!(message.contains(command.as_str()), "{message}");
    assert!(
        message.contains(&config.project.regenerate_command),
        "{message}"
    );
}

#[test]
fn write_mode_writes_only_changed_outputs_and_creates_directories() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let config = config_writing_into(directory.path());

    let mut written = generate(&config, Mode::Write).expect("writes");
    let mut expected = all_output_paths(&config);
    written.sort();
    expected.sort();
    assert_eq!(written, expected, "a first run writes every output");
    assert!(generate(&config, Mode::Check).expect("current").is_empty());

    assert!(
        generate(&config, Mode::Write).expect("writes").is_empty(),
        "a second run has nothing to write"
    );

    std::fs::write(&config.outputs.rust_model, "stale\n").expect("overwritten");
    assert_eq!(
        generate(&config, Mode::Write).expect("writes"),
        vec![config.outputs.rust_model.clone()]
    );
    assert!(generate(&config, Mode::Check).expect("current").is_empty());
}

#[test]
fn changing_the_definition_makes_check_name_exactly_the_outputs_it_changes() {
    let copy = copy_notebook();
    let config_path = copy.path().join("clerkenwell-codegen.json");
    let config = Config::load(&config_path).expect("the copy's configuration loads");
    assert!(generate(&config, Mode::Check)
        .expect("the copy is current")
        .is_empty());

    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/NoteSummary/properties/colour",
        json(r#"{ "type": "string", "description": "An en-GB field." }"#),
    );
    std::fs::write(&config.definition, document.stringify_pretty()).expect("rewritten");

    let definition = Definition::load(&config.definition).expect("still valid");
    let rendered = build_outputs(&definition, &config).expect("renders");
    let mut changed: Vec<PathBuf> = rendered
        .with_paths(&config.outputs)
        .into_iter()
        .filter(|(path, content)| std::fs::read_to_string(path).expect("committed") != *content)
        .map(|(path, _)| path.to_owned())
        .collect();
    assert!(!changed.is_empty(), "the change must reach some output");

    let mut stale = stale_paths(&config);
    stale.sort();
    changed.sort();
    assert_eq!(stale, changed);

    generate(&config, Mode::Write).expect("writes");
    assert!(generate(&config, Mode::Check)
        .expect("converged")
        .is_empty());
}

#[test]
fn the_cli_check_succeeds_silently_with_current_outputs() {
    let elsewhere = tempfile::tempdir().expect("a temporary directory");
    let output = Command::new(env!("CARGO_BIN_EXE_clerkenwell-codegen"))
        .arg("--config")
        .arg(notebook_config_path())
        .arg("--check")
        .current_dir(elsewhere.path())
        .output()
        .expect("the CLI runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn the_cli_check_fails_listing_stale_outputs() {
    let copy = copy_notebook();
    let config_path = copy.path().join("clerkenwell-codegen.json");
    let config = Config::load(&config_path).expect("the copy's configuration loads");
    std::fs::remove_file(&config.outputs.rust_model).expect("removed");

    let output = Command::new(env!("CARGO_BIN_EXE_clerkenwell-codegen"))
        .arg("--config")
        .arg(&config_path)
        .arg("--check")
        .output()
        .expect("the CLI runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&config.outputs.rust_model.display().to_string()),
        "{stderr}"
    );
    assert!(
        !config.outputs.rust_model.exists(),
        "check must not write the missing output"
    );
}

#[test]
fn configuration_paths_resolve_relative_to_the_configuration_file() {
    let config_path = notebook_config_path();
    let base = config_path
        .parent()
        .expect("the configuration sits in a directory");
    let raw = Json::parse(&std::fs::read_to_string(&config_path).expect("readable")).expect("JSON");
    let written = |pointer: &str| {
        raw.pointer(pointer)
            .and_then(Json::as_str)
            .unwrap_or_else(|| panic!("the configuration names {pointer}"))
    };
    let config = notebook_config();
    assert_eq!(config.definition, base.join(written("/definition")));
    let outputs = &config.outputs;
    for (key, resolved) in [
        ("collaborationFixtures", &outputs.collaboration_fixtures),
        ("contractFixtures", &outputs.contract_fixtures),
        ("normalizedDefinition", &outputs.normalized_definition),
        ("typescriptModel", &outputs.typescript_model),
        ("rustModel", &outputs.rust_model),
    ] {
        assert_eq!(resolved, &base.join(written(&format!("/outputs/{key}"))));
    }
}

#[test]
fn a_configuration_with_an_unknown_key_is_refused() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let text = std::fs::read_to_string(notebook_config_path()).expect("readable");
    let mut config = Json::parse(&text).expect("JSON");
    set(&mut config, "/outputs/pythonModel", json(r#""model.py""#));
    let path = directory.path().join("clerkenwell-codegen.json");
    std::fs::write(&path, config.stringify()).expect("written");
    match Config::load(&path) {
        Err(Error::Config { message, .. }) => assert!(message.contains("pythonModel"), "{message}"),
        other => panic!("expected a configuration refusal, got {other:?}"),
    }
}

#[test]
fn a_header_line_that_would_end_the_comment_is_refused() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let text = std::fs::read_to_string(notebook_config_path()).expect("readable");
    for line in ["closes */ early", "two\nlines"] {
        let mut config = Json::parse(&text).expect("JSON");
        set(&mut config, "/header", Json::Array(vec![line.into()]));
        let path = directory.path().join("clerkenwell-codegen.json");
        std::fs::write(&path, config.stringify()).expect("written");
        assert!(
            matches!(Config::load(&path), Err(Error::Config { .. })),
            "{line:?} was accepted"
        );
    }
}

/// Every project-specific name reaches the artefacts from the configuration.
#[test]
fn the_configured_header_and_command_open_every_generated_code_file() {
    let config = notebook_config();
    let definition = Definition::load(&config.definition).expect("valid");
    let outputs = build_outputs(&definition, &config).expect("renders");
    for code in [&outputs.rust_model, &outputs.typescript_model] {
        let opening: String = code.lines().take(8).collect::<Vec<_>>().join("\n");
        for line in &config.project.header {
            assert!(opening.contains(line.as_str()), "{opening}");
        }
        assert!(
            opening.contains(&format!("`{}`", config.project.regenerate_command)),
            "{opening}"
        );
    }
}

/// Every helper enum the Rust model names is declared: an inline string enum
/// as a record's values, as an open object's other members, and as the
/// values of a record-valued property, directly or within arrays and records
/// at any depth.
#[test]
fn the_enum_of_an_inline_typed_value_is_declared() {
    let document = with_defs(
        r#"{
            "ProbeRecord": { "type": "object",
                "additionalProperties": { "type": "string", "enum": ["low", "high"] } },
            "ProbeOpen": { "type": "object",
                "properties": { "level": { "type": "string", "enum": ["low"] } },
                "additionalProperties": { "type": "string", "enum": ["low", "high"] } },
            "ProbeHolder": { "type": "object", "additionalProperties": false,
                "properties": { "labels": { "type": "object",
                    "additionalProperties": { "type": "string", "enum": ["low", "high"] } },
                    "grid": { "type": "array", "items": { "type": "object",
                        "additionalProperties": { "type": "array",
                            "items": { "type": "string", "enum": ["low", "high"] } } } } } },
            "ProbeOpenList": { "type": "object",
                "properties": { "levels": { "type": "array",
                    "items": { "type": "string", "enum": ["low"] } } },
                "additionalProperties": { "type": "array",
                    "items": { "type": "string", "enum": ["low", "high"] } } },
            "ProbeRecordGrid": { "type": "object",
                "additionalProperties": { "type": "object",
                    "additionalProperties": { "type": "string", "enum": ["low", "high"] } } }
        }"#,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    for (named, declared) in [
        (
            "RecordMembers<ProbeRecordConstraints, ProbeRecordValue>",
            "pub enum ProbeRecordValue {",
        ),
        (
            "OtherMembers<ProbeOpen, ProbeOpenOtherMembers>",
            "pub enum ProbeOpenOtherMembers {",
        ),
        (
            "BTreeMap<String, ProbeHolderLabels>",
            "pub enum ProbeHolderLabels {",
        ),
        (
            "Vec<BTreeMap<String, Vec<ProbeHolderGrid>>>",
            "pub enum ProbeHolderGrid {",
        ),
        (
            "OtherMembers<ProbeOpenList, Vec<ProbeOpenListOtherMembers>>",
            "pub enum ProbeOpenListOtherMembers {",
        ),
        (
            "RecordMembers<ProbeRecordGridConstraints, BTreeMap<String, ProbeRecordGridValue>>",
            "pub enum ProbeRecordGridValue {",
        ),
    ] {
        assert!(rust.contains(named), "{named} is not named:\n{rust}");
        assert!(rust.contains(declared), "{declared} is not declared");
    }
}

/// Every object in `value`, at any depth, that holds `member`.
fn objects_holding<'a>(
    value: &'a serde_json::Value,
    member: &str,
    found: &mut Vec<&'a serde_json::Value>,
) {
    match value {
        serde_json::Value::Object(object) => {
            if object.contains_key(member) {
                found.push(value);
            }
            object
                .values()
                .for_each(|value| objects_holding(value, member, found));
        }
        serde_json::Value::Array(items) => items
            .iter()
            .for_each(|item| objects_holding(item, member, found)),
        _ => {}
    }
}

/// A contract fixture is a value of its definition: a sample honours the
/// constraints of each member it holds, an open object's other members
/// included, a union's sample is of a member its constraints leave values
/// of, and an open object whose other members are of its own type has a
/// finite sample.
#[test]
fn contract_fixtures_honour_constraints_and_recursive_open_objects() {
    let mut document = with_defs(
        r##"{
            "ProbeTree": { "type": "object",
                "properties": { "child": { "$ref": "#/$defs/ProbeTree" } },
                "additionalProperties": { "$ref": "#/$defs/ProbeTree" } },
            "ProbeConstrained": { "type": "object",
                "properties": {
                    "count": { "type": "integer", "minimum": 12, "maximum": 20 },
                    "code": { "type": "string", "minLength": 12 },
                    "long": { "type": "string", "minLength": 200000 },
                    "mode": { "type": "string", "const": "fixed" },
                    "ratio": { "type": "number", "minimum": 12.5, "maximum": 13 }
                },
                "required": ["count", "code", "mode", "ratio", "long"],
                "additionalProperties": { "$ref": "#/$defs/ProbeScalar" } },
            "ProbeScalar": { "type": ["integer", "string", "number"], "minimum": 10 },
            "ProbeLevel": { "type": "string", "enum": ["a", "valid"], "minLength": 2 },
            "ProbeFraction": { "type": ["integer", "number"], "minimum": 0.1, "maximum": 0.2 },
            "ProbeLevels": { "type": "object",
                "properties": { "first": { "$ref": "#/$defs/ProbeLevel" } },
                "required": ["first"],
                "additionalProperties": { "$ref": "#/$defs/ProbeLevel" } }
        }"##,
    );
    set(
        &mut document,
        "/$defs/NoteSummary/properties/tree",
        json(r##"{ "$ref": "#/$defs/ProbeTree" }"##),
    );
    set(
        &mut document,
        "/$defs/NoteSummary/properties/constrained",
        json(r##"{ "$ref": "#/$defs/ProbeConstrained" }"##),
    );
    push(
        &mut document,
        "/$defs/NoteSummary/required",
        json(r#""tree""#),
    );
    push(
        &mut document,
        "/$defs/NoteSummary/required",
        json(r#""constrained""#),
    );
    set(
        &mut document,
        "/$defs/NoteSummary/properties/levels",
        json(r##"{ "$ref": "#/$defs/ProbeLevels" }"##),
    );
    push(
        &mut document,
        "/$defs/NoteSummary/required",
        json(r#""levels""#),
    );
    set(
        &mut document,
        "/$defs/NoteSummary/properties/fraction",
        json(r##"{ "$ref": "#/$defs/ProbeFraction" }"##),
    );
    push(
        &mut document,
        "/$defs/NoteSummary/required",
        json(r#""fraction""#),
    );
    let definition = testing::definition(document).expect("valid");
    let outputs = build_outputs(&definition, &notebook_config()).expect("renders");
    let fixtures: serde_json::Value =
        serde_json::from_str(&outputs.contract_fixtures).expect("fixtures are JSON");

    let mut constrained = Vec::new();
    objects_holding(&fixtures, "ratio", &mut constrained);
    assert!(!constrained.is_empty(), "no sample of ProbeConstrained");
    for sample in constrained {
        let count = sample["count"].as_f64().expect("an integer count");
        assert!(
            (12.0..=20.0).contains(&count) && count.fract() == 0.0,
            "{sample}"
        );
        assert!(
            sample["code"].as_str().expect("a code").chars().count() >= 12,
            "{sample}"
        );
        assert!(
            sample["long"]
                .as_str()
                .expect("a long string")
                .chars()
                .count()
                >= 200_000,
            "a long string is too short"
        );
        assert_eq!(sample["mode"], "fixed");
        let ratio = sample["ratio"].as_f64().expect("a ratio");
        assert!((12.5..=13.0).contains(&ratio), "{sample}");
        for (name, value) in sample.as_object().expect("an object") {
            if ["count", "code", "mode", "ratio", "long"].contains(&name.as_str()) {
                continue;
            }
            if let Some(number) = value.as_f64() {
                assert!(number >= 10.0, "{name} is below the minimum in {sample}");
            }
        }
    }

    let mut fractions = Vec::new();
    objects_holding(&fixtures, "fraction", &mut fractions);
    assert!(!fractions.is_empty(), "no sample of ProbeFraction");
    for holder in fractions {
        let fraction = holder["fraction"].as_f64().expect("a number");
        assert!((0.1..=0.2).contains(&fraction), "{holder}");
    }

    let mut levels = Vec::new();
    objects_holding(&fixtures, "first", &mut levels);
    assert!(!levels.is_empty(), "no sample of ProbeLevels");
    for sample in levels {
        for (name, value) in sample.as_object().expect("an object") {
            assert_eq!(value, "valid", "{name} in {sample}");
        }
    }

    let mut trees = Vec::new();
    objects_holding(&fixtures, "tree", &mut trees);
    assert!(trees.iter().all(|holder| holder["tree"].is_object()));
}

/// A named string enum's validation keywords are transforms of its type, so
/// the schema derived for each use of it carries them.
#[test]
fn a_named_enum_carries_its_constraints() {
    let document = with_defs(
        r#"{ "ProbeLevel": { "type": "string", "enum": ["low", "high"], "minLength": 3 } }"#,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    assert!(
        rust.contains(
            r#"#[schemars(transform = SchemaConstraint::new("", "minLength", SchemaConstraintValue::Integer(3)))]
pub enum ProbeLevel {"#
        ),
        "{rust}"
    );
}

/// A `default` beside an open object's reference to its other members' type
/// is among its other-member constraints: the referenced type carries only
/// its own.
#[test]
fn a_default_beside_an_other_members_reference_is_a_constraint() {
    let document = with_defs(
        r##"{
            "ProbeLevel": { "type": "string", "enum": ["low", "high"] },
            "ProbeOpen": { "type": "object",
                "properties": { "first": { "$ref": "#/$defs/ProbeLevel" } },
                "additionalProperties": { "$ref": "#/$defs/ProbeLevel", "default": "high" } }
        }"##,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    let declared = rust
        .split("impl DeclaredMembers for ProbeOpen {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("ProbeOpen declares its members");
    assert!(declared.contains(r#"keyword: "default","#), "{declared}");
    assert!(
        declared.contains(r#"value: SchemaConstraintValue::String("high"),"#),
        "{declared}"
    );
}

/// A helper enum carries the validation keywords and default of the schema
/// it was declared from, so a record's or an open object's values derive
/// them though no field attribute reaches them.
#[test]
fn a_helper_enum_carries_its_schema_constraints() {
    let document = with_defs(
        r#"{ "ProbeRecord": { "type": "object", "additionalProperties":
            { "type": "string", "enum": ["low", "high"], "minLength": 3, "default": "low" } } }"#,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    assert!(
        rust.contains(
            r#"#[schemars(transform = SchemaConstraint::new("", "minLength", SchemaConstraintValue::Integer(3)))]
#[schemars(transform = SchemaConstraint::new("", "default", SchemaConstraintValue::String("low")))]
pub enum ProbeRecordValue {"#
        ),
        "{rust}"
    );
}

/// A fixture holds a string of at most 2^20 characters; generation refuses a
/// schema whose minLength asks for more rather than allocate it.
#[test]
fn a_fixture_string_longer_than_a_fixture_holds_is_refused() {
    for min_length in ["1048577", "1e100"] {
        let mut document = notebook_document();
        set(
            &mut document,
            "/$defs/NoteSummary/properties/title/minLength",
            json(min_length),
        );
        let definition = testing::definition(document).expect("valid");
        match build_outputs(&definition, &notebook_config()) {
            Err(Error::Definition(message)) => assert!(
                message.contains("minLength exceeds the 1048576 characters a fixture string holds"),
                "{message}"
            ),
            other => panic!("minLength {min_length}: expected a refusal, got {other:?}"),
        }
    }
}

/// The Rust type of a field that can reach its own struct again without an
/// indirection is boxed, whether it names the struct, an alias of it, or a
/// struct that refers back; other members are held in a map already.
#[test]
fn a_field_reaching_its_own_type_is_boxed() {
    let document = with_defs(
        r##"{
            "ProbeNode": { "type": "object", "additionalProperties": false,
                "properties": { "child": { "$ref": "#/$defs/ProbeNodeAlias" } } },
            "ProbeNodeAlias": { "$ref": "#/$defs/ProbeNode" },
            "ProbeLeft": { "type": "object", "additionalProperties": false,
                "properties": { "right": { "$ref": "#/$defs/ProbeRight" } } },
            "ProbeRight": { "type": "object", "additionalProperties": false,
                "properties": { "left": { "$ref": "#/$defs/ProbeLeft" } } },
            "ProbeTree": { "type": "object",
                "properties": { "child": { "$ref": "#/$defs/ProbeTreeAlias" } },
                "additionalProperties": { "$ref": "#/$defs/ProbeTree" } },
            "ProbeTreeAlias": { "$ref": "#/$defs/ProbeTree" }
        }"##,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    for field in [
        "pub child: Option<Box<ProbeNodeAlias>>,",
        "pub right: Option<Box<ProbeRight>>,",
        "pub left: Option<Box<ProbeLeft>>,",
        "pub child: Option<Box<ProbeTreeAlias>>,",
        "pub other_members: OtherMembers<ProbeTree, ProbeTree>,",
    ] {
        assert!(rust.contains(field), "{field} is not generated");
    }
}

/// An integer constraint at the edge of `i64` is written as the integer it
/// is, so the generated literal compiles.
#[test]
fn an_integer_constraint_at_the_i64_boundary_is_exact() {
    let document = with_defs(
        r#"{ "ProbeOpen": { "type": "object",
            "properties": { "member": { "type": "integer", "minimum": -9223372036854775808 } },
            "additionalProperties": { "type": "integer", "minimum": -9223372036854775808 } } }"#,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    assert!(
        rust.contains("value: SchemaConstraintValue::Integer(-9223372036854775808),"),
        "{rust}"
    );
    assert!(
        rust.contains(
            r#"SchemaConstraint::new("", "minimum", SchemaConstraintValue::Integer(-9223372036854775808))"#
        ),
        "the field bound is not exact"
    );
}

/// A string the Rust model writes is a Rust string literal, control
/// characters included.
#[test]
fn a_control_character_in_a_generated_string_is_a_rust_escape() {
    let document = with_defs(
        r#"{
            "ProbeOpen": { "type": "object",
                "properties": { "member": { "type": "string", "const": "\u0008x" } },
                "additionalProperties": { "type": "string", "const": "\u0008x",
                    "default": "\u0008x" } },
            "ProbeUnion": { "type": ["string", "integer"], "default": "\u000c" },
            "ProbeHolder": { "type": "object", "additionalProperties": false,
                "properties": { "label": { "type": "string", "default": "\u0008" } } }
        }"#,
    );
    let definition = testing::definition(document).expect("valid");
    assert_rustfmt_clean(&definition, &notebook_config());
}

/// The Rust model holds a JSON Schema integer as an `i64`, so an integer
/// fixture is one: generation refuses an integer schema whose bounds leave
/// it none, and a union samples another member instead.
#[test]
fn an_integer_fixture_is_an_i64() {
    let mut document =
        with_defs(r#"{ "ProbeWide": { "type": ["integer", "number"], "minimum": 1e19 } }"#);
    set(
        &mut document,
        "/$defs/NoteSummary/properties/wide",
        json(r##"{ "$ref": "#/$defs/ProbeWide" }"##),
    );
    push(
        &mut document,
        "/$defs/NoteSummary/required",
        json(r#""wide""#),
    );
    let definition = testing::definition(document).expect("valid");
    let outputs = build_outputs(&definition, &notebook_config()).expect("renders");
    let fixtures: serde_json::Value =
        serde_json::from_str(&outputs.contract_fixtures).expect("fixtures are JSON");
    let mut wide = Vec::new();
    objects_holding(&fixtures, "wide", &mut wide);
    assert!(!wide.is_empty(), "no sample of ProbeWide");
    for holder in wide {
        assert!(
            holder["wide"].as_f64().expect("a number") >= 1e19,
            "{holder}"
        );
    }

    let mut document = notebook_document();
    set(
        &mut document,
        "/$defs/NoteSummary/properties/huge",
        json(r#"{ "type": "integer", "minimum": 1e19 }"#),
    );
    push(
        &mut document,
        "/$defs/NoteSummary/required",
        json(r#""huge""#),
    );
    let definition = testing::definition(document).expect("valid");
    match build_outputs(&definition, &notebook_config()) {
        Err(Error::Definition(message)) => {
            assert!(
                message.contains("admit no integer an i64 holds"),
                "{message}"
            )
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A number the Rust model writes is a literal of a type that holds it: an
/// integer within `i64` as an integer, and any other number as a float.
#[test]
fn a_generated_number_is_a_literal_of_a_type_that_holds_it() {
    let document = with_defs(
        r#"{
            "ProbeSmall": { "type": ["integer", "number"], "default": 7 },
            "ProbeWide": { "type": ["integer", "number"], "default": 10000000000 },
            "ProbeHuge": { "type": ["integer", "number"], "default": 1e20, "minimum": 1e20 },
            "ProbeFraction": { "type": ["integer", "number"], "default": 2.5 }
        }"#,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    for literal in [
        r#""default", SchemaConstraintValue::Integer(7))"#,
        r#""default", SchemaConstraintValue::Integer(10000000000))"#,
        r#""minimum", SchemaConstraintValue::Number(1e20))"#,
        r#""default", SchemaConstraintValue::Number(1e20))"#,
        r#""default", SchemaConstraintValue::Number(2.5))"#,
    ] {
        assert!(rust.contains(literal), "{literal} is not generated");
    }
}

/// A record names its values' constraints, so a site referencing the record
/// repeats none of them.
#[test]
fn a_referenced_record_carries_its_value_constraints() {
    let document = with_defs(
        r##"{
            "ProbeRecord": { "type": "object",
                "additionalProperties": { "type": "string", "minLength": 2 } },
            "ProbeOpen": { "type": "object",
                "properties": { "first": { "$ref": "#/$defs/ProbeRecord" } },
                "additionalProperties": { "$ref": "#/$defs/ProbeRecord" } }
        }"##,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    let carried = rust
        .split("impl RecordConstraints for ProbeRecordConstraints {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("ProbeRecord names its constraints");
    assert!(
        carried.contains(r#"path: "","#)
            && carried.contains(r#"keyword: "minLength","#)
            && carried.contains("value: SchemaConstraintValue::Integer(2),"),
        "{carried}"
    );
    let declared = rust
        .split("impl DeclaredMembers for ProbeOpen {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("ProbeOpen declares its members");
    assert!(!declared.contains("OTHER_MEMBER_CONSTRAINTS"), "{declared}");
    let fields = rust
        .split("pub struct ProbeOpen {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("ProbeOpen is a struct");
    assert!(!fields.contains("SchemaConstraint"), "{fields}");
}

/// Every record carries the constraints of the scalars within its values, at
/// their JSON Pointers: directly, in array items and in nested records, as a
/// record `$defs` entry, as a record-valued property and within an array.
#[test]
fn a_record_carries_its_values_constraints_at_every_depth() {
    let document = with_defs(
        r#"{
            "ProbeRecord": { "type": "object",
                "additionalProperties": { "type": "string", "const": "x" } },
            "ProbeRecordList": { "type": "object",
                "additionalProperties": { "type": "array",
                    "items": { "type": "integer", "minimum": 1 } } },
            "ProbeRecordGrid": { "type": "object",
                "additionalProperties": { "type": "object",
                    "additionalProperties": { "type": "number", "maximum": 2.5 } } },
            "ProbeHolder": { "type": "object", "additionalProperties": false,
                "properties": {
                    "labels": { "type": "object",
                        "additionalProperties": { "type": "string", "minLength": 3 } },
                    "lists": { "type": "object",
                        "additionalProperties": { "type": "array",
                            "items": { "type": "string", "minLength": 4 } } },
                    "grid": { "type": "array", "items": { "type": "object",
                        "additionalProperties": { "type": "object",
                            "additionalProperties": { "type": "integer", "maximum": 5 } } } } } }
        }"#,
    );
    let definition = testing::definition(document).expect("valid");
    let rust = build_outputs(&definition, &notebook_config())
        .expect("renders")
        .rust_model;
    for (record, path, keyword, value) in [
        ("ProbeRecord", "", "const", r#"String("x")"#),
        ("ProbeRecordList", "/items", "minimum", "Integer(1)"),
        (
            "ProbeRecordGrid",
            "/additionalProperties",
            "maximum",
            "Number(2.5)",
        ),
    ] {
        let carried = rust
            .split(&format!(
                "impl RecordConstraints for {record}Constraints {{"
            ))
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .unwrap_or_else(|| panic!("{record} names its constraints"));
        for field in [
            format!("path: {path:?},"),
            format!("keyword: {keyword:?},"),
            format!("value: SchemaConstraintValue::{value},"),
        ] {
            assert!(carried.contains(&field), "{record}: {field}\n{carried}");
        }
    }
    for (path, keyword, value) in [
        ("/additionalProperties", "minLength", "Integer(3)"),
        ("/additionalProperties/items", "minLength", "Integer(4)"),
        (
            "/items/additionalProperties/additionalProperties",
            "maximum",
            "Integer(5)",
        ),
    ] {
        let attribute = format!(
            "#[schemars(transform = SchemaConstraint::new({path:?}, {keyword:?}, SchemaConstraintValue::{value}))]"
        );
        assert!(rust.contains(&attribute), "{attribute} is not generated");
    }
}

/// A record whose alias is wider than a line stays rustfmt-clean.
#[test]
fn a_record_with_a_long_name_satisfies_rustfmt() {
    let document = with_defs(
        r#"{ "ProbeRecordWhoseNameIsLongEnoughToFillAWholeLine": { "type": "object",
            "additionalProperties": { "type": "string", "minLength": 1 } } }"#,
    );
    let definition = testing::definition(document).expect("valid");
    assert_rustfmt_clean(&definition, &notebook_config());
}

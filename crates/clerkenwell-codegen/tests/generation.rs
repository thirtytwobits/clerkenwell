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
            assert_open_object_rustfmt_clean(&names[..count]);
        }
    }
}

fn assert_open_object_rustfmt_clean(names: &[&str]) {
    let properties: Vec<String> = names
        .iter()
        .map(|name| format!(r#""{name}": {{ "type": "string" }}"#))
        .collect();
    let document = with_defs(&format!(
        r#"{{ "ProbeOpen": {{ "type": "object", "properties": {{ {} }},
            "additionalProperties": {{ "type": "string" }} }} }}"#,
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

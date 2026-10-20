//! The example's generated Rust compiles, and the contract fixtures generated
//! beside it are values of its types.

mod common;

use clerkenwell_notebook as model;

use clerkenwell_codegen::testing::Json;
use common::{expanded_notebook, notebook_dir, notebook_document};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Parses `value` as `T` and serialises it back, as the JSON model sees it.
fn round_trip<T: DeserializeOwned + Serialize>(value: &Json) -> Json {
    let typed: T = serde_json::from_value(value.to_serde())
        .unwrap_or_else(|error| panic!("{} does not parse: {error}", value.stringify()));
    let text = serde_json::to_string(&typed).expect("serialises");
    Json::parse(&text).expect("serialised JSON parses")
}

fn contract_fixtures() -> Json {
    let text = std::fs::read_to_string(
        notebook_dir().join("generated/notebook.projections.fixtures.json"),
    )
    .expect("the contract fixtures are committed");
    Json::parse(&text).expect("the fixtures are JSON")
}

#[test]
fn every_contract_fixture_snapshot_and_patch_is_a_value_of_its_generated_type() {
    let fixtures = contract_fixtures();
    let projections = fixtures
        .pointer("/projections")
        .and_then(Json::as_array)
        .expect("fixtures list projections");
    assert!(!projections.is_empty());
    for projection in projections {
        let snapshot = projection.get("snapshot").expect("a snapshot");
        assert_eq!(
            &round_trip::<model::ProjectionTransportSnapshot>(snapshot),
            snapshot
        );
        for case in projection
            .get("cases")
            .and_then(Json::as_array)
            .expect("cases")
        {
            let patch = case.get("patch").expect("a patch");
            assert_eq!(&round_trip::<model::ProjectionTransportPatch>(patch), patch);
        }
    }
}

#[test]
fn every_public_name_parses_to_the_variant_that_spells_it() {
    for name in model::PROJECTION_NAMES {
        let parsed = model::ProjectionName::parse(name).expect("a projection name parses");
        assert_eq!(parsed.as_str(), *name);
    }
    for name in model::MUTATION_NAMES {
        let parsed = model::MutationName::parse(name).expect("a mutation name parses");
        assert_eq!(parsed.as_str(), *name);
    }
    for name in model::COMMAND_NAMES {
        let parsed = model::CommandName::parse(name).expect("a command name parses");
        assert_eq!(parsed.as_str(), *name);
    }
    assert!(model::CommandName::parse("not.a.command").is_none());
    assert!(model::ProjectionName::parse("not.a.projection").is_none());
}

#[test]
fn the_registries_list_every_definition_entry() {
    let document = expanded_notebook();
    let count = |pointer: &str| {
        document
            .pointer(pointer)
            .and_then(Json::as_object)
            .expect("a section")
            .len()
    };
    assert_eq!(
        model::GENERATED_PROJECTION_SPECS.len(),
        count("/projections")
    );
    assert_eq!(model::GENERATED_MUTATION_SPECS.len(), count("/mutations"));
    assert_eq!(model::GENERATED_COMMAND_SPECS.len(), count("/commands"));
    assert_eq!(
        model::GENERATED_ENTITY_AUTHORING_SPECS.len(),
        count("/entities")
    );
    assert_eq!(
        model::GENERATED_COLLABORATION_SPECS.len(),
        count("/collaboration/entities")
    );
    for spec in model::GENERATED_COLLABORATION_SPECS {
        let fields = count(&format!("/collaboration/entities/{}/fields", spec.name));
        assert_eq!(spec.fields.len(), fields, "{}", spec.name);
        assert!(model::generated_collaboration_spec(spec.name).is_some());
    }
}

/// Validation keywords reach the JSON Schema a Rust consumer derives, with the
/// values the definition declares.
#[test]
fn declared_constraints_reach_the_derived_json_schema() {
    let document = notebook_document();
    let derived = |schema: schemars::Schema| {
        Json::parse(&serde_json::to_string(&schema).expect("serialises")).expect("JSON")
    };
    let declared = |pointer: &str| {
        document
            .pointer(pointer)
            .cloned()
            .unwrap_or_else(|| panic!("the example declares {pointer}"))
    };

    let settings = derived(schemars::schema_for!(model::WorkspaceSettings));
    for (property, keyword) in [
        ("scope", "const"),
        ("theme", "default"),
        ("shortcuts", "default"),
        ("autosave_seconds", "default"),
    ] {
        assert_eq!(
            settings.pointer(&format!("/properties/{property}/{keyword}")),
            Some(&declared(&format!(
                "/$defs/WorkspaceSettings/properties/{property}/{keyword}"
            ))),
            "{property}.{keyword}"
        );
    }

    let task = derived(schemars::schema_for!(model::TaskRecord));
    assert_eq!(
        task.pointer("/properties/title/minLength"),
        Some(&declared("/$defs/TaskRecord/properties/title/minLength"))
    );
    assert_eq!(
        task.pointer("/properties/checklist/items/minLength"),
        Some(&declared(
            "/$defs/TaskRecord/properties/checklist/items/minLength"
        ))
    );
}

#[test]
fn each_command_spec_names_the_entities_its_command_touches() {
    let document = notebook_document();
    for spec in model::GENERATED_COMMAND_SPECS {
        let declared: Vec<&str> = document
            .pointer(&format!(
                "/commands/{}/touches",
                spec.name.replace('/', "~1")
            ))
            .and_then(Json::as_array)
            .map(|touches| touches.iter().filter_map(Json::as_str).collect())
            .unwrap_or_default();
        assert_eq!(spec.touches, declared.as_slice(), "{}", spec.name);
    }
}

#[test]
fn a_command_result_on_the_wire_names_its_command() {
    let archived = 3;
    let result =
        model::ProjectionTransportCommandResult::TaskArchiveDone(model::TaskArchiveDoneResult {
            archived,
        });

    let wire = serde_json::to_value(&result).expect("serialises");
    assert_eq!(wire["command"], model::TASK_ARCHIVE_DONE_COMMAND);
    assert_eq!(wire["value"]["archived"], archived);

    let read: model::ProjectionTransportCommandResult =
        serde_json::from_value(wire).expect("deserialises");
    assert_eq!(read.command_name(), model::CommandName::TaskArchiveDone);
    assert_eq!(read, result);
}

/// A record serialises its keys in key order, whatever order they were read in,
/// both as a record `$defs` entry and as a record field.
#[test]
fn a_record_serialises_its_keys_in_key_order() {
    let keys = ["zeta", "alpha", "mu", "beta"];
    let mut sorted = keys;
    sorted.sort_unstable();
    let entries = |order: &[&str]| -> String {
        let pairs: Vec<String> = order
            .iter()
            .map(|key| format!(r#""{key}":"{key}""#))
            .collect();
        format!("{{{}}}", pairs.join(","))
    };

    let properties: model::NoteProperties =
        serde_json::from_str(&entries(&keys)).expect("a record parses");
    assert_eq!(
        serde_json::to_string(&properties).expect("serialises"),
        entries(&sorted)
    );

    let settings: model::WorkspaceSettings = serde_json::from_str(&format!(
        r#"{{"scope":"workspace","revision":"r","theme":"system","shortcuts":{},"autosave_seconds":30}}"#,
        entries(&keys)
    ))
    .expect("settings parse");
    let wire = serde_json::to_string(&settings).expect("serialises");
    assert!(
        wire.contains(&format!(r#""shortcuts":{}"#, entries(&sorted))),
        "{wire}"
    );
}

/// An open object reads and writes its declared members and every other
/// member, and its other members never hold a declared one.
#[test]
fn an_open_object_round_trips_every_member() {
    let wire = serde_json::json!({
        "theme": "dark",
        "accent": "teal",
        "density": "compact",
        "font": "serif",
    });
    let appearance: model::NoteAppearance =
        serde_json::from_value(wire.clone()).expect("an open object parses");
    assert_eq!(appearance.theme, model::NoteAppearanceTheme::Dark);
    assert_eq!(appearance.accent.as_deref(), Some("teal"));
    assert_eq!(
        appearance.other_members.keys().collect::<Vec<_>>(),
        ["density", "font"]
    );
    assert_eq!(serde_json::to_value(&appearance).expect("serialises"), wire);

    let mut other_members = model::OtherMembers::<model::NoteAppearance, String>::new();
    assert!(other_members.insert("theme", "light".to_owned()).is_err());
    other_members
        .insert("density", "roomy".to_owned())
        .expect("density is not declared");
    let built = model::NoteAppearance {
        theme: model::NoteAppearanceTheme::Light,
        accent: None,
        other_members,
    };
    assert_eq!(
        serde_json::to_value(&built).expect("serialises"),
        serde_json::json!({ "theme": "light", "density": "roomy" })
    );
}

/// The schema `schemars` derives for an open object declares its members and
/// types the others.
#[test]
fn an_open_object_schema_declares_its_members_and_types_the_others() {
    let schema =
        serde_json::to_value(schemars::schema_for!(model::NoteAppearance)).expect("a schema");
    let properties = schema["properties"].as_object().expect("properties");
    let mut declared: Vec<&String> = properties.keys().collect();
    declared.sort_unstable();
    assert_eq!(declared, ["accent", "theme"]);
    assert_eq!(schema["additionalProperties"]["type"], "string");
    assert_eq!(schema["required"], serde_json::json!(["theme"]));
}

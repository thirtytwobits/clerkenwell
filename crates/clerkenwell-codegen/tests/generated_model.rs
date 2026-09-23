//! The example's generated Rust compiles, and the contract fixtures generated
//! beside it are values of its types.

mod common;

#[allow(dead_code, clippy::all)]
#[path = "fixtures/notebook/generated/rust/model.rs"]
mod model;

use clerkenwell_codegen::json::Json;
use common::{notebook_dir, notebook_document};
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
    assert!(model::ProjectionName::parse("not.a.projection").is_none());
}

#[test]
fn the_registries_list_every_definition_entry() {
    let document = notebook_document();
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

//! The generator declares every collaborative entity's authoring state: its
//! projection, the parameters that name its document, and the snapshot and
//! patch every authoring state shares.

mod common;

use clerkenwell_codegen::testing::{self, Json};
use clerkenwell_codegen::Definition;
use common::{json, merge, notebook_document, refusal, set, validate};

fn collaborative_entities(definition: &Definition) -> Vec<(String, String)> {
    testing::document(definition)
        .get("collaboration")
        .and_then(|collaboration| collaboration.pointer("/entities"))
        .and_then(Json::as_object)
        .expect("the notebook collaborates")
        .iter()
        .map(|(name, entity)| {
            let projection = entity
                .pointer("/authoringState/projection")
                .and_then(Json::as_str)
                .expect("an authoring state");
            (name.to_owned(), projection.to_owned())
        })
        .collect()
}

/// The names of the string parameters `definition` requires of `entity`'s
/// authoring state.
fn required_params(definition: &Definition, entity: &str) -> Vec<String> {
    let params = testing::document(definition)
        .get("$defs")
        .and_then(|defs| defs.get(&format!("{entity}AuthoringParams")))
        .and_then(Json::as_object)
        .unwrap_or_else(|| panic!("{entity}'s authoring parameters are declared"));
    let required: Vec<String> = params
        .get("required")
        .and_then(Json::as_array)
        .expect("required parameters")
        .iter()
        .map(|name| name.as_str().expect("a parameter name").to_owned())
        .collect();
    for name in &required {
        assert_eq!(
            params
                .get("properties")
                .and_then(|properties| properties.pointer(&format!("/{name}/type")))
                .and_then(Json::as_str),
            Some("string"),
            "{entity}'s parameter {name} is a string"
        );
    }
    required
}

fn entity_id(definition: &Definition, entity: &str) -> String {
    testing::document(definition)
        .get("entities")
        .and_then(|entities| entities.pointer(&format!("/{entity}/id")))
        .and_then(Json::as_str)
        .expect("a declared entity")
        .to_owned()
}

#[test]
fn every_collaborative_entity_has_an_authoring_state_projection_of_the_shared_shape() {
    let definition = validate(notebook_document()).expect("the notebook is valid");
    let entities = collaborative_entities(&definition);
    assert!(!entities.is_empty());
    for (entity, name) in entities {
        let projection = testing::document(&definition)
            .get("projections")
            .and_then(|projections| projections.get(&name))
            .unwrap_or_else(|| panic!("{name} is declared"));
        let reference = |pointer: &str| {
            projection
                .pointer(pointer)
                .and_then(Json::as_str)
                .unwrap_or_else(|| panic!("{name}{pointer} is a reference"))
                .to_owned()
        };
        assert_eq!(reference("/snapshot/$ref"), "#/$defs/AuthoringState");
        assert_eq!(reference("/patch/$ref"), "#/$defs/AuthoringStatePatch");
        assert_eq!(
            reference("/params/$ref"),
            format!("#/$defs/{entity}AuthoringParams")
        );
        assert_eq!(
            projection.pointer("/dependsOn"),
            Some(&Json::Array(vec![Json::String(entity.clone())]))
        );
    }
    let defs = testing::document(&definition)
        .get("$defs")
        .and_then(Json::as_object)
        .expect("the definition declares $defs");
    assert!(defs.get("AuthoringState").is_some());
    assert!(defs.get("AuthoringStatePatch").is_some());
}

#[test]
fn an_authoring_state_names_its_document_by_the_entity_id() {
    let definition = validate(notebook_document()).expect("the notebook is valid");
    for (entity, _) in collaborative_entities(&definition) {
        assert_eq!(
            required_params(&definition, &entity),
            [entity_id(&definition, &entity)]
        );
    }
}

#[test]
fn an_entity_with_one_document_takes_no_id_parameter() {
    let mut document = notebook_document();
    set(
        &mut document,
        "/collaboration/entities/Note/authoringState/document",
        json(r#""notebook""#),
    );

    let definition = validate(document).expect("valid");

    assert!(required_params(&definition, "Note").is_empty());
}

#[test]
fn the_parameters_that_choose_a_store_follow_the_id_parameter() {
    let mut document = notebook_document();
    let store_params = ["session_id", "shard"];
    set(
        &mut document,
        "/collaboration/entities/Note/authoringState/storeParams",
        Json::Array(
            store_params
                .iter()
                .map(|name| Json::String((*name).into()))
                .collect(),
        ),
    );

    let definition = validate(document).expect("valid");

    let mut expected = vec![entity_id(&definition, "Note")];
    expected.extend(store_params.iter().map(|name| (*name).to_owned()));
    assert_eq!(required_params(&definition, "Note"), expected);
}

#[test]
fn a_store_parameter_may_not_be_the_id_parameter() {
    let mut document = notebook_document();
    let id = validate(notebook_document())
        .map(|definition| entity_id(&definition, "Note"))
        .expect("valid");
    set(
        &mut document,
        "/collaboration/entities/Note/authoringState/storeParams",
        Json::Array(vec![Json::String(id)]),
    );

    let message = refusal(document);

    assert!(
        message.contains("collaboration.entities.Note.authoringState.storeParams"),
        "{message}"
    );
}

#[test]
fn the_declarations_the_generator_writes_are_refused_when_written_by_hand() {
    for name in [
        "AuthoringState",
        "AuthoringStatePatch",
        "NoteAuthoringParams",
    ] {
        let mut document = notebook_document();
        merge(
            &mut document,
            "/$defs",
            json(&format!(r#"{{ "{name}": {{ "type": "string" }} }}"#)),
        );

        let message = refusal(document);

        assert!(message.contains(&format!("$defs.{name}")), "{message}");
    }
}

#[test]
fn two_entities_may_not_share_an_authoring_state() {
    let mut document = notebook_document();
    let definition = validate(notebook_document()).expect("valid");
    let entities = collaborative_entities(&definition);
    let [(first, shared), (second, _), ..] = entities.as_slice() else {
        panic!("the notebook collaborates on two entities");
    };
    set(
        &mut document,
        &format!("/collaboration/entities/{second}/authoringState/projection"),
        Json::String(shared.clone()),
    );

    let message = refusal(document);

    assert!(
        message.contains(&format!(
            "collaboration.entities.{second}.authoringState.projection"
        )) && message.contains(first.as_str()),
        "{message}"
    );
}

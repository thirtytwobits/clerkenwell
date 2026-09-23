//! The normalised IR: the definition with every named section as a sorted list
//! and every object's keys sorted, so it is identical however the source was
//! ordered.

use crate::collate::locale_compare;
use crate::definition::{Definition, Fields};
use crate::json::{Json, Object};

/// The canonical document every later stage can consume.
pub fn normalized_document(definition: &Definition) -> Json {
    let collaboration = definition.collaboration();
    let collaboration_entities: Vec<Json> =
        sorted_entries(collaboration.raw.object_field("entities"))
            .into_iter()
            .map(|(name, entity)| {
                let entity = entity
                    .as_object()
                    .expect("the meta-schema requires objects");
                let fields: Vec<Json> = sorted_entries(entity.object_field("fields"))
                    .into_iter()
                    .map(|(path, field)| deep_sort(&named("path", path, field)))
                    .collect();
                let mut object = named("name", name, &Json::Object(entity.clone()));
                object
                    .as_object_mut()
                    .expect("named builds an object")
                    .insert("fields", Json::Array(fields));
                deep_sort(&object)
            })
            .collect();

    let mut collaboration_document = Object::new();
    collaboration_document.insert("version", collaboration.version().into());
    collaboration_document.insert(
        "compatibility",
        collaboration.compatibility().clone().into(),
    );
    collaboration_document.insert("entities", Json::Array(collaboration_entities));

    let document = definition.document();
    let mut normalized = Object::new();
    normalized.insert("version", definition.version().into());
    normalized.insert("compatibility", definition.compatibility().clone().into());
    normalized.insert("namespace", definition.namespace().into());
    for (key, section) in [
        ("entities", "entities"),
        ("projections", "projections"),
        ("mutations", "mutations"),
        ("mnemonic", "mnemonic"),
    ] {
        normalized.insert(key, named_entries(document.object_field(section)));
    }
    normalized.insert("collaboration", collaboration_document.into());
    normalized.insert("definitions", named_entries(definition.defs()));
    deep_sort(&Json::Object(normalized))
}

/// The normalised IR file: pretty-printed with a trailing newline.
pub fn render_normalized_definition(definition: &Definition) -> String {
    format!("{}\n", normalized_document(definition).stringify_pretty())
}

fn sorted_entries(section: &Object) -> Vec<(&str, &Json)> {
    let mut entries: Vec<(&str, &Json)> = section.iter().collect();
    entries.sort_by(|(left, _), (right, _)| locale_compare(left, right));
    entries
}

/// `{ [key]: name, ...value }`.
fn named(key: &str, name: &str, value: &Json) -> Json {
    let mut object = Object::new();
    object.insert(key, name.into());
    if let Some(value) = value.as_object() {
        object.spread(value);
    }
    Json::Object(object)
}

fn named_entries(section: &Object) -> Json {
    Json::Array(
        sorted_entries(section)
            .into_iter()
            .map(|(name, value)| deep_sort(&named("name", name, value)))
            .collect(),
    )
}

/// `value` with every object's keys in locale order.
fn deep_sort(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(deep_sort).collect()),
        Json::Object(object) => {
            let mut entries: Vec<(&str, &Json)> = object.iter().collect();
            entries.sort_by(|(left, _), (right, _)| locale_compare(left, right));
            Json::Object(
                entries
                    .into_iter()
                    .map(|(key, child)| (key, deep_sort(child)))
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

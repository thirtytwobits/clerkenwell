//! The JSON Schema documents a definition's schemas denote, which the
//! generated specs carry.
//!
//! The definition's subset is JSON Schema but for two forms: `"type": "json"`
//! is any JSON value, and a tagged union is a `oneOf` object mapping each tag
//! to its variant, with `discriminator` naming the property that carries the
//! tag.

use std::collections::HashSet;

use crate::definition::Definition;
use crate::json::{Json, Object};
use crate::schema::local_reference_name;

/// The JSON Schema document a reference the meta-schema has shaped names: the
/// definition it resolves to, with the `$defs` entries that definition
/// reaches, in the definition's order.
pub(crate) fn schema_document(definition: &Definition, reference: &Object) -> Json {
    let root = def(
        definition,
        local_reference_name(reference.get("$ref")).expect("validation resolved the reference"),
    );
    let mut reached = HashSet::new();
    let mut pending = vec![root];
    while let Some(schema) = pending.pop() {
        let mut names = Vec::new();
        references(schema, &mut names);
        for name in names {
            if reached.insert(name) {
                pending.push(def(definition, name));
            }
        }
    }
    let defs: Object = definition
        .def_entries()
        .filter(|(name, _)| reached.contains(name))
        .map(|(name, schema)| (name, Json::Object(json_schema(schema))))
        .collect();
    let mut document = json_schema(root);
    if !defs.is_empty() {
        document.insert("$defs", Json::Object(defs));
    }
    Json::Object(document)
}

fn def<'d>(definition: &'d Definition, name: &str) -> &'d Object {
    definition
        .def(name)
        .expect("validation resolved every reference")
}

/// The schemas nested in `schema`.
fn subschemas(schema: &Object) -> impl Iterator<Item = &Object> {
    let maps = ["properties", "oneOf"]
        .into_iter()
        .filter_map(|key| schema.get(key).and_then(Json::as_object))
        .flat_map(|map| map.iter().map(|(_, value)| value));
    ["additionalProperties", "items"]
        .into_iter()
        .filter_map(|key| schema.get(key))
        .chain(maps)
        .filter_map(Json::as_object)
}

/// Every `#/$defs` name `schema` and the schemas nested in it reference.
fn references<'s>(schema: &'s Object, names: &mut Vec<&'s str>) {
    if let Some(name) = local_reference_name(schema.get("$ref")) {
        names.push(name);
    }
    for subschema in subschemas(schema) {
        references(subschema, names);
    }
}

/// `schema` as JSON Schema.
fn json_schema(schema: &Object) -> Object {
    let discriminator = schema.get("discriminator").and_then(Json::as_str);
    let mut converted = Object::new();
    for (key, value) in schema.iter() {
        match (key, value) {
            ("type", Json::String(kind)) if kind == "json" => {}
            ("discriminator", _) => {}
            ("properties", Json::Object(properties)) => {
                converted.insert(key, Json::Object(each_json_schema(properties)));
            }
            ("additionalProperties" | "items", Json::Object(subschema)) => {
                converted.insert(key, Json::Object(json_schema(subschema)));
            }
            ("oneOf", Json::Object(variants)) => {
                let discriminator =
                    discriminator.expect("validation requires a discriminator beside oneOf");
                let variants = variants
                    .iter()
                    .map(|(tag, variant)| {
                        let variant = variant
                            .as_object()
                            .expect("validation requires object variants");
                        Json::Object(tagged_variant(discriminator, tag, variant))
                    })
                    .collect();
                converted.insert(key, Json::Array(variants));
            }
            _ => converted.insert(key, value.clone()),
        }
    }
    converted
}

fn each_json_schema(schemas: &Object) -> Object {
    schemas
        .iter()
        .map(|(name, schema)| {
            let schema = schema
                .as_object()
                .expect("validation requires object schemas");
            (name, Json::Object(json_schema(schema)))
        })
        .collect()
}

/// A tagged union's variant as JSON Schema: the variant, with the
/// discriminator a required property whose value is the tag.
fn tagged_variant(discriminator: &str, tag: &str, variant: &Object) -> Object {
    let mut properties = Object::new();
    properties.insert(discriminator, Json::Object(constant(tag)));
    if let Some(declared) = variant.get("properties").and_then(Json::as_object) {
        properties.spread(&each_json_schema(declared));
    }
    let mut required = vec![Json::from(discriminator)];
    if let Some(declared) = variant.get("required").and_then(Json::as_array) {
        required.extend(declared.iter().cloned());
    }
    let mut tagged = json_schema(variant);
    tagged.insert("properties", Json::Object(properties));
    tagged.insert("required", Json::Array(required));
    tagged
}

fn constant(value: &str) -> Object {
    let mut schema = Object::new();
    schema.insert("const", Json::from(value));
    schema
}

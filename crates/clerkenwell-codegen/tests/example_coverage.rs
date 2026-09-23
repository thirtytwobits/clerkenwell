//! The notebook example exercises everything the definition language admits,
//! so its committed outputs cover every rendering path.

mod common;

use std::collections::BTreeSet;

use clerkenwell_codegen::json::Json;
use clerkenwell_codegen::{schema_keywords, META_SCHEMA};
use common::notebook_document;

/// The values the meta-schema's enum at `pointer` admits.
fn admitted(pointer: &str) -> BTreeSet<String> {
    let meta_schema = Json::parse(META_SCHEMA).expect("the meta-schema is JSON");
    meta_schema
        .pointer(pointer)
        .and_then(Json::as_array)
        .unwrap_or_else(|| panic!("the meta-schema declares an enum at {pointer}"))
        .iter()
        .map(|value| value.as_str().expect("string enum").to_owned())
        .collect()
}

/// Every string found at `field` in the objects `pointer` holds.
fn used(document: &Json, pointer: &str, field: &str) -> BTreeSet<String> {
    let section = document
        .pointer(pointer)
        .and_then(Json::as_object)
        .unwrap_or_else(|| panic!("{pointer} is an object"));
    section
        .values()
        .filter_map(|entry| entry.pointer(field).and_then(Json::as_str))
        .map(str::to_owned)
        .collect()
}

fn collaboration_fields(document: &Json) -> Vec<&Json> {
    document
        .pointer("/collaboration/entities")
        .and_then(Json::as_object)
        .expect("the example collaborates")
        .values()
        .flat_map(|entity| {
            entity
                .get("fields")
                .and_then(Json::as_object)
                .expect("every collaboration entity has fields")
                .values()
        })
        .collect()
}

fn assert_covers(what: &str, admitted: BTreeSet<String>, used: BTreeSet<String>) {
    let missing: Vec<_> = admitted.difference(&used).collect();
    assert!(missing.is_empty(), "the example uses no {what} {missing:?}");
}

#[test]
fn every_collaboration_storage_kind_codec_and_conflict_policy_is_used() {
    let document = notebook_document();
    let fields = collaboration_fields(&document);
    let collect = |pointer: &str| -> BTreeSet<String> {
        fields
            .iter()
            .filter_map(|field| field.pointer(pointer).and_then(Json::as_str))
            .map(str::to_owned)
            .collect()
    };
    assert_covers(
        "storage kind",
        admitted("/definitions/collaborationStorage/properties/kind/enum"),
        collect("/storage/kind"),
    );
    assert_covers(
        "value codec",
        admitted("/definitions/collaborationValue/properties/codec/enum"),
        collect("/value/codec"),
    );
    assert_covers(
        "conflict policy",
        admitted("/definitions/collaborationField/properties/conflict/enum"),
        collect("/conflict"),
    );
    let storage_settings: BTreeSet<String> = fields
        .iter()
        .flat_map(|field| {
            field
                .pointer("/storage")
                .and_then(Json::as_object)
                .expect("storage is an object")
                .keys()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    let meta_schema = Json::parse(META_SCHEMA).expect("JSON");
    let admitted_settings: BTreeSet<String> = meta_schema
        .pointer("/definitions/collaborationStorage/properties")
        .and_then(Json::as_object)
        .expect("storage properties")
        .keys()
        .map(str::to_owned)
        .collect();
    assert_covers("storage setting", admitted_settings, storage_settings);
    assert!(
        fields
            .iter()
            .any(|field| field.get("required") == Some(&Json::Bool(false))),
        "the example has no optional collaboration field"
    );
}

#[test]
fn every_entity_projection_and_mnemonic_policy_is_used() {
    let document = notebook_document();
    assert_covers(
        "authoring kind",
        admitted("/definitions/authoringPolicy/properties/kind/enum"),
        used(&document, "/entities", "/authoring/kind"),
    );
    assert_covers(
        "revision kind",
        admitted("/definitions/revision/properties/kind/enum"),
        used(&document, "/entities", "/revision/kind"),
    );
    assert_covers(
        "materialisation strategy",
        admitted("/definitions/materialization/properties/strategy/enum"),
        used(&document, "/projections", "/materialization/strategy"),
    );
    assert_covers(
        "snapshot mode",
        admitted("/definitions/materialization/properties/snapshotMode/enum"),
        used(&document, "/projections", "/materialization/snapshotMode"),
    );
    assert_covers(
        "remove mode",
        admitted("/definitions/materialization/properties/removeMode/enum"),
        used(&document, "/projections", "/materialization/removeMode"),
    );
    assert_covers(
        "cache policy",
        admitted("/definitions/mnemonic/properties/cachePolicy/enum"),
        used(&document, "/mnemonic", "/cachePolicy"),
    );
    assert_covers(
        "mnemonic recovery",
        admitted("/definitions/mnemonic/properties/recovery/enum"),
        used(&document, "/mnemonic", "/recovery"),
    );
    let settings: BTreeSet<String> = document
        .pointer("/projections")
        .and_then(Json::as_object)
        .expect("projections")
        .values()
        .flat_map(|projection| {
            projection
                .pointer("/materialization")
                .and_then(Json::as_object)
                .expect("materialization")
                .keys()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    let meta_schema = Json::parse(META_SCHEMA).expect("JSON");
    let admitted_settings: BTreeSet<String> = meta_schema
        .pointer("/definitions/materialization/properties")
        .and_then(Json::as_object)
        .expect("materialization properties")
        .keys()
        .map(str::to_owned)
        .collect();
    assert_covers("materialisation setting", admitted_settings, settings);
}

/// Every schema node in `$defs`, with each node's own keywords.
fn schema_nodes(schema: &Json, nodes: &mut Vec<Json>) {
    let Some(object) = schema.as_object() else {
        return;
    };
    nodes.push(schema.clone());
    for key in ["properties", "oneOf"] {
        if let Some(children) = object.get(key).and_then(Json::as_object) {
            for child in children.values() {
                schema_nodes(child, nodes);
            }
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(child) = object.get(key) {
            schema_nodes(child, nodes);
        }
    }
}

#[test]
fn every_schema_keyword_and_type_is_used() {
    let document = notebook_document();
    let mut nodes = Vec::new();
    for schema in document
        .pointer("/$defs")
        .and_then(Json::as_object)
        .expect("$defs")
        .values()
    {
        schema_nodes(schema, &mut nodes);
    }
    let keywords: BTreeSet<String> = nodes
        .iter()
        .flat_map(|node| {
            node.as_object()
                .expect("nodes are objects")
                .keys()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    assert_covers(
        "schema keyword",
        schema_keywords().map(str::to_owned).collect(),
        keywords,
    );

    let types: BTreeSet<String> = nodes
        .iter()
        .filter_map(|node| node.get("type").and_then(Json::as_str))
        .map(str::to_owned)
        .collect();
    assert_covers(
        "schema type",
        [
            "object", "array", "string", "boolean", "integer", "number", "json",
        ]
        .map(str::to_owned)
        .into(),
        types,
    );
    assert!(
        nodes
            .iter()
            .any(|node| node.get("type").and_then(Json::as_array).is_some()),
        "the example declares no primitive union"
    );
    assert!(
        nodes.iter().any(|node| node
            .get("additionalProperties")
            .and_then(Json::as_object)
            .is_some()),
        "the example declares no record"
    );
    for keyword in ["const", "default"] {
        let kinds: BTreeSet<&str> = nodes
            .iter()
            .filter_map(|node| node.get(keyword))
            .map(|value| match value {
                Json::String(_) => "string",
                Json::Number(_) => "number",
                Json::Bool(_) => "boolean",
                Json::Array(_) => "array",
                Json::Object(_) => "object",
                Json::Null => "null",
            })
            .collect();
        let expected: BTreeSet<&str> = if keyword == "const" {
            ["string", "number", "boolean"].into()
        } else {
            ["string", "number", "boolean", "array", "object"].into()
        };
        assert!(
            expected.is_subset(&kinds),
            "the example's {keyword} values are only {kinds:?}"
        );
    }
}

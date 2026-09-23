//! The two fixture corpora both language bindings are tested against: one
//! snapshot and one patch per patch kind for every projection, and one sample
//! document with its operations for every collaborative entity.

use crate::definition::{CollaborationEntity, CollaborationField, Definition, Materialization};
use crate::error::{refuse, Result};
use crate::json::{Json, Object};
use crate::names::{kebab_identifier, snake_to_camel_identifier};
use crate::schema::{
    has_type, object_at, primitive_union_types, read_string_array, resolve_ref,
    stringify_or_undefined,
};
use crate::validate::contract_property;

/// The collaboration fixture corpus.
pub fn render_collaboration_fixtures(definition: &Definition) -> Result<String> {
    let collaboration = definition.collaboration();
    let mut entities = Vec::new();
    for entity in collaboration.entities() {
        entities.push(collaboration_entity_fixture(definition, entity)?.into());
    }
    let mut corpus = Object::new();
    corpus.insert(
        "collaborationDefinitionVersion",
        collaboration.version().into(),
    );
    corpus.insert("entities", Json::Array(entities));
    Ok(format!("{}\n", Json::Object(corpus).stringify_pretty()))
}

fn collaboration_entity_fixture(
    definition: &Definition,
    entity: CollaborationEntity,
) -> Result<Object> {
    let fields: Vec<CollaborationField> = entity.fields().collect();
    let sequence_fields: Vec<&CollaborationField> = fields
        .iter()
        .filter(|field| field.storage_kind() == "keyedSequence")
        .collect();
    let mut document = Object::new();
    for field in &fields {
        if field.path.contains(".*.")
            || field.is_derived()
            || field.storage_kind() == "keyedSequence"
        {
            continue;
        }
        let value = collaboration_fixture_value(definition, entity.name, field, "value")?;
        set_fixture_path(&mut document, field.path, value);
    }
    for sequence in direct_child_sequences(&fields, None) {
        let items = sequence_items(definition, entity.name, &fields, sequence)?;
        set_fixture_path(&mut document, sequence.path, Json::Array(items));
    }
    // A derived field is written by whoever owns the document, never left out
    // by a client: the substrate refuses a sequence item with no identity.
    let optional_paths: Vec<&str> = fields
        .iter()
        .filter(|field| !field.required() && !field.is_derived())
        .map(|field| field.path)
        .collect();

    let mut scalar = Object::new();
    scalar.insert_some(
        "path",
        fields
            .iter()
            .find(|field| field.storage_kind() == "scalar" && field.conflict() == "explicit")
            .map(|field| field.path.into()),
    );
    scalar.insert("value", "updated-value".into());
    let sequence_operation = |key: &str, value: Json| -> Json {
        Json::Array(
            sequence_fields
                .iter()
                .map(|field| {
                    let mut operation = Object::new();
                    operation.insert("path", field.path.into());
                    operation.insert(key, value.clone());
                    operation.into()
                })
                .collect(),
        )
    };
    let mut operations = Object::new();
    operations.insert("scalar", scalar.into());
    operations.insert(
        "reorder",
        sequence_operation("order", Json::Array(vec![1.0.into(), 0.0.into()])),
    );
    operations.insert("delete", sequence_operation("index", 0.0.into()));
    operations.insert("optionalPresent", optional_paths.clone().into());
    operations.insert("optionalAbsent", optional_paths.into());

    let first_sequence = sequence_fields.first().map(|field| Json::from(field.path));
    let invalid_sequence = |kind: &str| -> Json {
        let mut invalid = Object::new();
        invalid.insert("kind", kind.into());
        invalid.insert_some("sequencePath", first_sequence.clone());
        invalid.into()
    };
    let mut unsupported_version = Object::new();
    unsupported_version.insert("kind", "unsupportedSchemaVersion".into());
    unsupported_version.insert("schemaVersion", (entity.schema_version() + 1.0).into());

    let mut fixture = Object::new();
    fixture.insert("entity", entity.name.into());
    fixture.insert("schemaVersion", entity.schema_version().into());
    fixture.insert(
        "migrationIds",
        entity
            .raw
            .get("migrationIds")
            .cloned()
            .unwrap_or(Json::Null),
    );
    let client_document = map_fixture_keys(&Json::Object(document.clone()));
    fixture.insert("wireDocument", document.into());
    fixture.insert("clientDocument", client_document);
    fixture.insert("operations", operations.into());
    fixture.insert(
        "invalid",
        Json::Array(vec![
            invalid_sequence("missingIdentity"),
            invalid_sequence("duplicateIdentity"),
            unsupported_version.into(),
        ]),
    );
    Ok(fixture)
}

fn collaboration_fixture_value(
    definition: &Definition,
    entity_name: &str,
    field: &CollaborationField,
    suffix: &str,
) -> Result<Json> {
    let path = field.path;
    Ok(match field.codec() {
        "integer" => 1.0.into(),
        "number" | "optionalNumber" => 1.5.into(),
        "boolean" => true.into(),
        "string" | "optionalString" => {
            let declared = collaboration_field_declared_schema(definition, entity_name, path)?;
            match declared
                .and_then(|schema| schema.get("enum"))
                .and_then(Json::as_array)
            {
                Some(values) => values.first().cloned().unwrap_or(Json::Null),
                None => format!("{}-{suffix}", kebab_identifier(path)).into(),
            }
        }
        "propertyText" => {
            let mut value = Object::new();
            value.insert("$mime", "text/markdown".into());
            value.insert("value", format!("{path} {suffix}").into());
            value.into()
        }
        "stringList" => Json::Array(vec![format!("{}-{suffix}", kebab_identifier(path)).into()]),
        "structuredJson" => {
            let reference = field
                .value_schema()
                .expect("validation requires a structuredJson schema");
            let schema = resolve_ref(
                definition,
                reference,
                &format!("collaboration field {path}"),
            )?;
            let sample =
                populate_fixture_strings(&sample_schema_value(definition, schema)?, suffix);
            if !matches!(field.storage_kind(), "structuredMap" | "structuredDocument") {
                Json::Array(vec![sample])
            } else if sample.as_object().is_some_and(Object::is_empty) {
                open_map_fixture_sample(definition, schema, path, suffix)?
            } else {
                sample
            }
        }
        "identity" | "keyedSequence" => format!("{}-{suffix}", kebab_identifier(path)).into(),
        other => unreachable!("the meta-schema admits no codec {other:?}"),
    })
}

/// An open map sampled from declared properties alone is `{}`, which leaves the
/// field's merge behaviour without coverage: emit one entry shaped by
/// `additionalProperties`.
fn open_map_fixture_sample(
    definition: &Definition,
    schema: &Object,
    path: &str,
    suffix: &str,
) -> Result<Json> {
    let stem = kebab_identifier(path);
    let entry = match object_at(schema, "additionalProperties") {
        Some(additional) if !has_type(additional, "json") => {
            populate_fixture_strings(&sample_schema_value(definition, additional)?, suffix)
        }
        _ => format!("{stem}-{suffix}").into(),
    };
    let mut sample = Object::new();
    sample.insert(format!("{stem}-key"), entry);
    Ok(sample.into())
}

/// Follows `$ref` until it reaches a schema that is not one.
fn resolve_fixture_schema_refs<'a>(
    definition: &'a Definition,
    schema: &'a Object,
) -> Result<&'a Object> {
    let mut current = schema;
    while current.get("$ref").is_some() {
        current = resolve_ref(definition, current, "collaboration entity schema")?;
    }
    Ok(current)
}

/// The entity schema property a collaboration field path names, so fixtures
/// can honour constraints the codec alone does not carry.
fn collaboration_field_declared_schema<'a>(
    definition: &'a Definition,
    entity_name: &str,
    path: &str,
) -> Result<Option<&'a Object>> {
    let Some(entity) = definition.entity(entity_name) else {
        return Ok(None);
    };
    let mut current = resolve_fixture_schema_refs(definition, entity.schema())?;
    for segment in path.split('.') {
        if segment == "*" {
            let Some(items) = object_at(current, "items") else {
                return Ok(None);
            };
            current = resolve_fixture_schema_refs(definition, items)?;
            continue;
        }
        let Some(property) =
            object_at(current, "properties").and_then(|properties| object_at(properties, segment))
        else {
            return Ok(None);
        };
        current = resolve_fixture_schema_refs(definition, property)?;
    }
    Ok(Some(current))
}

fn sequence_items(
    definition: &Definition,
    entity_name: &str,
    fields: &[CollaborationField],
    sequence: &CollaborationField,
) -> Result<Vec<Json>> {
    let identity_path = sequence
        .storage_setting("identityPath")
        .expect("validation requires a keyedSequence identityPath");
    let prefix = format!("{}.*.", sequence.path);
    let mut items = Vec::new();
    for suffix in ["one", "two"] {
        let mut item = Object::new();
        set_fixture_path(
            &mut item,
            identity_path,
            format!("{}-{suffix}", kebab_identifier(entity_name)).into(),
        );
        for field in direct_item_fields(fields, sequence.path) {
            if field.is_derived() {
                continue;
            }
            let value = collaboration_fixture_value(definition, entity_name, field, suffix)?;
            set_fixture_path(&mut item, &field.path[prefix.len()..], value);
        }
        for child in direct_child_sequences(fields, Some(sequence.path)) {
            let child_items = sequence_items(definition, entity_name, fields, child)?;
            set_fixture_path(
                &mut item,
                &child.path[prefix.len()..],
                Json::Array(child_items),
            );
        }
        items.push(item.into());
    }
    Ok(items)
}

/// Keyed sequences directly under `parent` (the document root when `None`).
fn direct_child_sequences<'f, 'a>(
    fields: &'f [CollaborationField<'a>],
    parent: Option<&str>,
) -> Vec<&'f CollaborationField<'a>> {
    fields
        .iter()
        .filter(|field| {
            if field.storage_kind() != "keyedSequence" {
                return false;
            }
            match parent {
                None => !field.path.contains(".*."),
                Some(parent) => {
                    let prefix = format!("{parent}.*.");
                    field.path.starts_with(&prefix) && !field.path[prefix.len()..].contains(".*.")
                }
            }
        })
        .collect()
}

/// Non-sequence fields of one item of the sequence at `sequence_path`.
fn direct_item_fields<'f, 'a>(
    fields: &'f [CollaborationField<'a>],
    sequence_path: &str,
) -> Vec<&'f CollaborationField<'a>> {
    let prefix = format!("{sequence_path}.*.");
    fields
        .iter()
        .filter(|field| {
            field.storage_kind() != "keyedSequence"
                && field.path.starts_with(&prefix)
                && !field.path[prefix.len()..].contains(".*.")
        })
        .collect()
}

/// Replaces every sampled string with one naming its key and the fixture suffix.
fn populate_fixture_strings(value: &Json, suffix: &str) -> Json {
    match value {
        Json::Array(items) => Json::Array(
            items
                .iter()
                .map(|item| populate_fixture_strings(item, suffix))
                .collect(),
        ),
        Json::String(_) => format!("{suffix}-value").into(),
        Json::Object(object) => Json::Object(
            object
                .iter()
                .map(|(key, child)| {
                    let populated = match child {
                        Json::String(_) => format!("{}-{suffix}", kebab_identifier(key)).into(),
                        other => populate_fixture_strings(other, suffix),
                    };
                    (key, populated)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Assigns `value` at the dotted `path`, replacing whatever non-object stands
/// in the way.
fn set_fixture_path(root: &mut Object, path: &str, value: Json) {
    let mut segments: Vec<&str> = path.split('.').collect();
    let last = segments.pop().expect("split yields at least one segment");
    let mut current = root;
    for segment in segments {
        if !matches!(current.get(segment), Some(Json::Object(_))) {
            current.insert(segment, Object::new().into());
        }
        current = current
            .get_mut(segment)
            .and_then(Json::as_object_mut)
            .expect("inserted above");
    }
    current.insert(last, value);
}

/// The client spelling of a wire document: keys camel-cased, `$` keys kept.
fn map_fixture_keys(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(map_fixture_keys).collect()),
        Json::Object(object) => Json::Object(
            object
                .iter()
                .map(|(key, child)| {
                    let key = if key.starts_with('$') {
                        key.to_owned()
                    } else {
                        snake_to_camel_identifier(key)
                    };
                    (key, map_fixture_keys(child))
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The projection contract fixture corpus.
pub fn render_contract_fixtures(definition: &Definition) -> Result<String> {
    let mut projections = Vec::new();
    for projection in definition.projections() {
        let name = projection.name;
        let snapshot_schema = resolve_ref(
            definition,
            projection.snapshot(),
            &format!("projections.{name}.snapshot"),
        )?;
        let patch_schema = resolve_ref(
            definition,
            projection.patch(),
            &format!("projections.{name}.patch"),
        )?;
        let materialization = projection.materialization();
        let snapshot = prepare_fixture_snapshot(
            sample_schema_value(definition, snapshot_schema)?,
            materialization,
        );
        let kind_schema = contract_property(
            patch_schema,
            "kind",
            &format!("projections.{name}.patch.kind"),
        )?;
        let kinds = read_string_array(
            kind_schema.get("enum"),
            &format!("projections.{name}.patch.kind.enum"),
        )?;
        let mut cases = Vec::new();
        for kind in kinds {
            let updates_field = materialization.setting("updatesField").filter(|field| {
                materialization.strategy() == "replaceOrRemove" && !field.is_empty()
            });
            let sampled_changes = match updates_field {
                Some(field) => sample_schema_value(
                    definition,
                    contract_property(patch_schema, field, "update changes")?,
                )?,
                None => Object::new().into(),
            };
            let (patch, expected) =
                projection_fixture_case(materialization, kind, &snapshot, &sampled_changes)?;
            let mut transported = Object::new();
            transported.insert("projection", name.into());
            transported.insert("value", patch.into());
            let mut case = Object::new();
            case.insert("kind", kind.into());
            case.insert("patch", transported.into());
            case.insert("expectedSnapshot", expected);
            cases.push(case.into());
        }
        let mut transported = Object::new();
        transported.insert("projection", name.into());
        transported.insert("value", snapshot);
        let mut fixture = Object::new();
        fixture.insert("projection", name.into());
        fixture.insert("strategy", materialization.strategy().into());
        fixture.insert("snapshot", transported.into());
        fixture.insert("cases", Json::Array(cases));
        projections.push(fixture.into());
    }
    let mut corpus = Object::new();
    corpus.insert("version", 1.0.into());
    corpus.insert("projections", Json::Array(projections));
    Ok(format!("{}\n", Json::Object(corpus).stringify_pretty()))
}

/// A keyed or sequenced snapshot gets exactly one item with a known identity.
fn prepare_fixture_snapshot(sampled: Json, plan: Materialization) -> Json {
    let strategy = plan.strategy();
    if strategy != "keyedCollection" && strategy != "sequencedText" {
        return sampled;
    }
    let mut snapshot = sampled.as_object().cloned().unwrap_or_default();
    let collection_field = plan.required("collectionField");
    let mut item = first_object_item(snapshot.get(collection_field))
        .cloned()
        .unwrap_or_default();
    item.insert(plan.required("itemIdentityField"), "fixture-id".into());
    if strategy == "sequencedText" {
        item.insert(
            plan.required("snapshotOutputField"),
            empty_output(plan).into(),
        );
    }
    snapshot.insert(collection_field, Json::Array(vec![item.into()]));
    snapshot.into()
}

fn empty_output(plan: Materialization) -> Object {
    let mut output = Object::new();
    output.insert(plan.required("sequenceField"), 0.0.into());
    output.insert(plan.required("textField"), "".into());
    output
}

fn first_object_item(collection: Option<&Json>) -> Option<&Object> {
    collection?.as_array()?.first()?.as_object()
}

/// A sample value of `schema`: required properties only, the first enum value,
/// the first union member, the first tagged variant.
fn sample_schema_value(definition: &Definition, schema: &Object) -> Result<Json> {
    if schema.get("$ref").is_some() {
        return sample_schema_value(
            definition,
            resolve_ref(definition, schema, "fixture schema")?,
        );
    }
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        return Ok(values.first().cloned().unwrap_or(Json::Null));
    }
    if let Some(members) = primitive_union_types(schema)? {
        let mut member = Object::new();
        member.insert("type", members[0].into());
        return sample_schema_value(definition, &member);
    }
    let schema_type = schema.get("type");
    Ok(match schema_type.and_then(Json::as_str) {
        Some("string") => "fixture".into(),
        Some("integer" | "number") => 1.0.into(),
        Some("boolean") => true.into(),
        Some("json") => Json::Null,
        Some("array") => {
            let items = object_at(schema, "items").expect("validation requires array items");
            Json::Array(vec![sample_schema_value(definition, items)?])
        }
        Some("object") => {
            if let Some(variants) = object_at(schema, "oneOf") {
                let (tag, variant) = variants
                    .iter()
                    .next()
                    .expect("validation requires a variant");
                let discriminator = schema
                    .get("discriminator")
                    .and_then(Json::as_str)
                    .expect("validation requires a discriminator");
                let mut sample = Object::new();
                sample.insert(discriminator, tag.into());
                let variant = variant
                    .as_object()
                    .expect("validation requires object variants");
                if let Json::Object(fields) = sample_schema_value(definition, variant)? {
                    sample.spread(&fields);
                }
                return Ok(sample.into());
            }
            let required = read_string_array(schema.get("required"), "fixture required")?;
            let mut sample = Object::new();
            if let Some(properties) = object_at(schema, "properties") {
                for (property, property_schema) in properties.iter() {
                    if required.contains(&property) {
                        let property_schema = property_schema
                            .as_object()
                            .expect("validation requires object property schemas");
                        sample.insert(property, sample_schema_value(definition, property_schema)?);
                    }
                }
            }
            sample.into()
        }
        _ => {
            return refuse(format!(
                "Cannot generate fixture for schema type {}.",
                stringify_or_undefined(schema_type)
            ))
        }
    })
}

/// One patch of `kind` and the snapshot applying it to `snapshot` yields.
fn projection_fixture_case(
    plan: Materialization,
    kind: &str,
    sampled_snapshot: &Json,
    sampled_changes: &Json,
) -> Result<(Object, Json)> {
    let snapshot = sampled_snapshot.as_object().cloned().unwrap_or_default();
    let mut patch = Object::new();
    patch.insert("kind", kind.into());
    match plan.strategy() {
        "keyedCollection" => {
            let collection_field = plan.required("collectionField");
            let item: Json = match first_object_item(snapshot.get(collection_field)) {
                Some(item) => item.clone().into(),
                None => {
                    let mut item = Object::new();
                    item.insert(plan.required("itemIdentityField"), "fixture-id".into());
                    item.into()
                }
            };
            let mut keyed_snapshot = snapshot.clone();
            keyed_snapshot.insert(collection_field, Json::Array(vec![item.clone()]));
            match kind {
                "reset" => {
                    patch.insert(collection_field, Json::Array(vec![item]));
                    Ok((patch, keyed_snapshot.into()))
                }
                "upsert" => {
                    patch.insert(plan.required("itemField"), item);
                    Ok((patch, keyed_snapshot.into()))
                }
                "remove" => {
                    patch.insert(plan.required("patchIdentityField"), "fixture-id".into());
                    keyed_snapshot.insert(collection_field, Json::Array(Vec::new()));
                    Ok((patch, keyed_snapshot.into()))
                }
                _ => refuse(format!("Unsupported keyed fixture patch kind {kind}.")),
            }
        }
        "sequencedText" => {
            let collection_field = plan.required("collectionField");
            let item_identity_field = plan.required("itemIdentityField");
            let output_field = plan.required("snapshotOutputField");
            let sequence_field = plan.required("sequenceField");
            let text_field = plan.required("textField");
            let item = match first_object_item(snapshot.get(collection_field)) {
                Some(item) => item.clone(),
                None => {
                    let mut item = Object::new();
                    item.insert(item_identity_field, "fixture-id".into());
                    item.insert(output_field, empty_output(plan).into());
                    item
                }
            };
            let mut stream_snapshot = snapshot.clone();
            stream_snapshot.insert(collection_field, Json::Array(vec![item.clone().into()]));
            match kind {
                "reset" => {
                    patch.spread(&stream_snapshot);
                    Ok((patch, stream_snapshot.into()))
                }
                "output" => {
                    let current_output = item.get(output_field).and_then(Json::as_object);
                    let sequence = current_output
                        .and_then(|output| output.get(sequence_field))
                        .and_then(Json::as_f64)
                        .map_or(1.0, |sequence| sequence + 1.0);
                    let text = current_output
                        .and_then(|output| output.get(text_field))
                        .and_then(Json::as_str)
                        .unwrap_or("");
                    patch.insert_some(
                        plan.required("patchIdentityField"),
                        item.get(item_identity_field).cloned(),
                    );
                    let mut delta = Object::new();
                    delta.insert(sequence_field, sequence.into());
                    delta.insert(plan.required("deltaTextField"), "fixture".into());
                    patch.insert(plan.required("patchOutputField"), delta.into());
                    let mut output = Object::new();
                    output.insert(sequence_field, sequence.into());
                    output.insert(text_field, format!("{text}fixture").into());
                    let mut next_item = item.clone();
                    next_item.insert(output_field, output.into());
                    let mut expected = stream_snapshot.clone();
                    expected.insert(collection_field, Json::Array(vec![next_item.into()]));
                    Ok((patch, expected.into()))
                }
                _ => refuse(format!(
                    "Unsupported sequenced-text fixture patch kind {kind}."
                )),
            }
        }
        "replaceOrRemove" => {
            if kind == "update" {
                let update_field = plan.required("updateField");
                let changes: Object = sampled_changes
                    .as_object()
                    .map(|changes| {
                        changes
                            .iter()
                            .map(|(key, value)| {
                                let value = if value.as_array().is_some() {
                                    Json::Array(Vec::new())
                                } else {
                                    value.clone()
                                };
                                (key, value)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut patch = patch;
                patch.insert(plan.required("updatesField"), changes.clone().into());
                let mut target = snapshot
                    .get(update_field)
                    .and_then(Json::as_object)
                    .cloned()
                    .unwrap_or_default();
                target.spread(&changes);
                let mut expected = snapshot.clone();
                expected.insert(update_field, target.into());
                return Ok((patch, expected.into()));
            }
            if kind == "replace" {
                return Ok((patch_for_snapshot(plan, &snapshot), snapshot.into()));
            }
            if plan.setting("removeMode") == Some("nullSnapshot") {
                return Ok((patch, Json::Null));
            }
            let mut expected = snapshot.clone();
            expected.insert(plan.required("removeField"), Json::Null);
            Ok((patch, expected.into()))
        }
        "replace" => Ok((patch_for_snapshot(plan, &snapshot), snapshot.into())),
        "reset" => {
            patch.spread(&snapshot);
            Ok((patch, snapshot.into()))
        }
        other => unreachable!("the meta-schema admits no strategy {other:?}"),
    }
}

fn patch_for_snapshot(plan: Materialization, snapshot: &Object) -> Object {
    let mut patch = Object::new();
    patch.insert("kind", "replace".into());
    if plan.setting("snapshotMode") == Some("field") {
        patch.insert(plan.required("snapshotField"), snapshot.clone().into());
        return patch;
    }
    let omitted = plan.snapshot_omit_fields().unwrap_or_default();
    for (field, value) in snapshot.iter() {
        if !omitted.contains(&field) {
            patch.insert(field, value.clone());
        }
    }
    patch
}

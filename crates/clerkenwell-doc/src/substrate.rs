use std::collections::{HashMap, HashSet};

use clerkenwell_schema::{
    GeneratedCollaborationEntitySpec, GeneratedCollaborationFieldSpec,
    GeneratedCollaborationStorageKind, GeneratedCollaborationValueCodec,
};
use loro::{Container, LoroDoc, LoroList, LoroMap, LoroValue, ToJson, ValueOrContainer};
use serde_json::{Map, Value};
use similar::algorithms::{myers, Capture};
use similar::DiffOp;

use crate::CollaborationLoroError;

type IdentityContext = HashMap<String, String>;

/// Where property text without a stored MIME type takes one from: the same
/// path in `document` when that holds a `text/` MIME type, else `default_mime`.
#[derive(Debug, Clone, Copy)]
struct MimeSource<'a> {
    document: &'a Value,
    default_mime: &'a str,
}

const STRUCTURED_MAP_PRESENCE_CONTAINER: &str = "collaboration.structured_map_presence";
const KEYED_SEQUENCE_PRESENCE_CONTAINER: &str = "collaboration.keyed_sequence_presence";

pub(crate) fn declared_text(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    field_path: &str,
    context: &IdentityContext,
) -> Result<loro::LoroText, CollaborationLoroError> {
    let field = plan
        .fields
        .iter()
        .find(|field| field.path == field_path)
        .filter(|field| field.storage_kind == GeneratedCollaborationStorageKind::Text)
        .ok_or_else(|| invalid_field(field_path, "a declared collaborative text field"))?;
    for sequence in plan.fields.iter().filter(|candidate| {
        candidate.storage_kind == GeneratedCollaborationStorageKind::KeyedSequence
            && field_path.starts_with(&format!("{}.*.", candidate.path))
    }) {
        let identity = context
            .get(identity_variable(sequence))
            .ok_or_else(|| invalid_field(field_path, "resolved record identities"))?;
        let order = resolve_template(
            sequence.path,
            sequence
                .order_container
                .ok_or_else(|| invalid_field(sequence.path, "an order container"))?,
            context,
        )?;
        if !unique_loro_string_list(&doc.get_list(order.as_str())).contains(identity) {
            return Err(invalid_field(field_path, "an existing record"));
        }
    }
    Ok(doc.get_text(resolve_container(field, context)?.as_str()))
}

pub(crate) fn validate_document(
    plan: &GeneratedCollaborationEntitySpec,
    document: &Value,
) -> Result<(), CollaborationLoroError> {
    document
        .as_object()
        .ok_or_else(|| invalid_field("$", "an object"))?;
    for field in plan.fields {
        if field.path.contains(".*.")
            || matches!(
                field.storage_kind,
                GeneratedCollaborationStorageKind::DerivedIdentity
                    | GeneratedCollaborationStorageKind::DerivedRevision
            )
        {
            continue;
        }
        if field.storage_kind != GeneratedCollaborationStorageKind::KeyedSequence && field.required
        {
            validate_field_value(field, value_at_path(document, field.path))?;
        }
    }
    for sequence in direct_child_sequences(plan, None) {
        validate_sequence(plan, document, sequence, &IdentityContext::new())?;
    }
    Ok(())
}

pub(crate) fn write_document_changes(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    previous: &Value,
    next: &Value,
) -> Result<(), CollaborationLoroError> {
    validate_document(plan, next)?;
    for field in plan.fields {
        if field.path.contains(".*.")
            || matches!(
                field.storage_kind,
                GeneratedCollaborationStorageKind::DerivedIdentity
                    | GeneratedCollaborationStorageKind::DerivedRevision
                    | GeneratedCollaborationStorageKind::KeyedSequence
            )
        {
            continue;
        }
        write_field_if_changed(
            doc,
            field,
            value_at_path(previous, field.path),
            value_at_path(next, field.path),
            &IdentityContext::new(),
        )?;
    }

    for sequence in direct_child_sequences(plan, None) {
        write_sequence_changes(plan, doc, sequence, previous, next, &IdentityContext::new())?;
    }
    Ok(())
}

pub(crate) fn materialize_document(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    revision: &str,
) -> Result<Value, CollaborationLoroError> {
    materialize_document_internal(plan, doc, revision, None)
}

pub(crate) fn materialize_document_resolving_text_mime(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    revision: &str,
    mime_source: &Value,
    default_mime: &str,
) -> Result<Value, CollaborationLoroError> {
    materialize_document_internal(
        plan,
        doc,
        revision,
        Some(MimeSource {
            document: mime_source,
            default_mime,
        }),
    )
}

fn materialize_document_internal(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    revision: &str,
    mime_source: Option<MimeSource<'_>>,
) -> Result<Value, CollaborationLoroError> {
    let mut document = Value::Object(Map::new());
    for field in plan.fields {
        if field.path.contains(".*.")
            || field.storage_kind == GeneratedCollaborationStorageKind::KeyedSequence
        {
            continue;
        }
        match field.storage_kind {
            GeneratedCollaborationStorageKind::DerivedIdentity => {}
            GeneratedCollaborationStorageKind::DerivedRevision => {
                set_value_at_path(
                    &mut document,
                    field.path,
                    Value::String(revision.to_string()),
                )?;
            }
            _ => {
                if let Some(value) = read_field(doc, field, &IdentityContext::new(), mime_source)? {
                    if !(is_empty_optional_value(&value)
                        && !field.required
                        && !is_object_storage_kind(field.storage_kind))
                    {
                        set_value_at_path(&mut document, field.path, value)?;
                    }
                }
            }
        }
    }
    for sequence in direct_child_sequences(plan, None) {
        let items =
            materialize_sequence(plan, doc, sequence, &IdentityContext::new(), mime_source)?;
        if sequence.required
            || !items.is_empty()
            || sequence_is_explicitly_present(doc, sequence, &IdentityContext::new())?
        {
            set_value_at_path(&mut document, sequence.path, Value::Array(items))?;
        }
    }
    complete_present_optional_groups(plan, &mut document)?;
    validate_document(plan, &document)?;
    Ok(document)
}

fn materialize_sequence(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    sequence: &GeneratedCollaborationFieldSpec,
    context: &IdentityContext,
    mime_source: Option<MimeSource<'_>>,
) -> Result<Vec<Value>, CollaborationLoroError> {
    let order_template = sequence
        .order_container
        .ok_or_else(|| invalid_field(sequence.path, "an order container"))?;
    let order_container = resolve_template(sequence.path, order_template, context)?;
    let identities = unique_loro_string_list(&doc.get_list(order_container.as_str()));
    let prefix = format!("{}.*.", sequence.path);
    let mut items = Vec::with_capacity(identities.len());
    for identity in identities {
        let mut item = Value::Object(Map::new());
        let mut item_context = context.clone();
        item_context.insert(identity_variable(sequence).to_string(), identity.clone());
        for field in direct_sequence_item_fields(plan, sequence) {
            let item_path = field
                .path
                .strip_prefix(&prefix)
                .expect("filtered item field");
            match field.storage_kind {
                GeneratedCollaborationStorageKind::DerivedIdentity => {
                    let variable = field
                        .identity_variable
                        .or(field.identity_path)
                        .unwrap_or(identity_variable(sequence));
                    let value = item_context
                        .get(variable)
                        .ok_or_else(|| invalid_field(field.path, "a resolved identity"))?;
                    set_value_at_path(&mut item, item_path, Value::String(value.clone()))?;
                }
                GeneratedCollaborationStorageKind::DerivedRevision => {}
                _ => {
                    if let Some(value) = read_field(doc, field, &item_context, mime_source)? {
                        if !(is_empty_optional_value(&value)
                            && !field.required
                            && !is_object_storage_kind(field.storage_kind))
                        {
                            set_value_at_path(&mut item, item_path, value)?;
                        }
                    }
                }
            }
        }
        for child in direct_child_sequences(plan, Some(sequence.path)) {
            let child_items = materialize_sequence(plan, doc, child, &item_context, mime_source)?;
            if child.required
                || !child_items.is_empty()
                || sequence_is_explicitly_present(doc, child, &item_context)?
            {
                let child_path = child
                    .path
                    .strip_prefix(&prefix)
                    .expect("direct child sequence");
                set_value_at_path(&mut item, child_path, Value::Array(child_items))?;
            }
        }
        items.push(item);
    }
    Ok(items)
}

fn complete_present_optional_groups(
    plan: &GeneratedCollaborationEntitySpec,
    document: &mut Value,
) -> Result<(), CollaborationLoroError> {
    for field in plan.fields.iter().filter(|field| {
        !field.required
            && !field.path.contains(".*.")
            && !matches!(
                field.storage_kind,
                GeneratedCollaborationStorageKind::DerivedIdentity
                    | GeneratedCollaborationStorageKind::DerivedRevision
                    | GeneratedCollaborationStorageKind::KeyedSequence
            )
    }) {
        if value_at_path(document, field.path).is_some() {
            continue;
        }
        let Some((parent_path, _)) = field.path.rsplit_once('.') else {
            continue;
        };
        if value_at_path(document, parent_path).is_none() {
            continue;
        }
        let default = match field.codec {
            GeneratedCollaborationValueCodec::OptionalString => Value::String(String::new()),
            GeneratedCollaborationValueCodec::StringList => Value::Array(Vec::new()),
            GeneratedCollaborationValueCodec::StructuredJson
                if !is_object_storage_kind(field.storage_kind) =>
            {
                Value::Array(Vec::new())
            }
            _ => continue,
        };
        set_value_at_path(document, field.path, default)?;
    }
    Ok(())
}

fn is_empty_optional_value(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty)
        || value.as_object().is_some_and(Map::is_empty)
        || value.as_str().is_some_and(str::is_empty)
        || value.is_null()
}

fn validate_sequence(
    plan: &GeneratedCollaborationEntitySpec,
    parent: &Value,
    sequence: &GeneratedCollaborationFieldSpec,
    context: &IdentityContext,
) -> Result<(), CollaborationLoroError> {
    let relative_path = sequence_relative_path(plan, sequence);
    let items: &[Value] = match value_at_path(parent, relative_path) {
        Some(value) => value
            .as_array()
            .ok_or_else(|| invalid_field(sequence.path, "an array"))?
            .as_slice(),
        None if !sequence.required => &[],
        None => return Err(invalid_field(sequence.path, "an array")),
    };
    let identity_path = sequence
        .identity_path
        .ok_or_else(|| invalid_field(sequence.path, "an identity path"))?;
    let prefix = format!("{}.*.", sequence.path);
    let mut identities = HashSet::new();
    for item in items {
        let identity = value_at_path(item, identity_path)
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_field(identity_path, "a string"))?;
        if identity.is_empty() || !identities.insert(identity) {
            return Err(invalid_field(
                sequence.path,
                "items with unique non-empty identities",
            ));
        }
        let mut item_context = context.clone();
        item_context.insert(
            identity_variable(sequence).to_string(),
            identity.to_string(),
        );
        for field in direct_sequence_item_fields(plan, sequence) {
            if !field.required
                || matches!(
                    field.storage_kind,
                    GeneratedCollaborationStorageKind::DerivedIdentity
                        | GeneratedCollaborationStorageKind::DerivedRevision
                )
            {
                continue;
            }
            validate_field_value(
                field,
                value_at_path(
                    item,
                    field
                        .path
                        .strip_prefix(&prefix)
                        .expect("filtered item field"),
                ),
            )?;
        }
        for child in direct_child_sequences(plan, Some(sequence.path)) {
            validate_sequence(plan, item, child, &item_context)?;
        }
    }
    Ok(())
}

fn write_sequence_changes(
    plan: &GeneratedCollaborationEntitySpec,
    doc: &LoroDoc,
    sequence: &GeneratedCollaborationFieldSpec,
    previous_parent: &Value,
    next_parent: &Value,
    context: &IdentityContext,
) -> Result<(), CollaborationLoroError> {
    let identity_path = sequence
        .identity_path
        .ok_or_else(|| invalid_field(sequence.path, "an identity path"))?;
    let relative_path = sequence_relative_path(plan, sequence);
    let previous_items = keyed_items(previous_parent, relative_path, identity_path)?;
    let next_items = keyed_items(next_parent, relative_path, identity_path)?;
    let previous_order = previous_items
        .iter()
        .map(|(identity, _)| identity.clone())
        .collect::<Vec<_>>();
    let next_order = next_items
        .iter()
        .map(|(identity, _)| identity.clone())
        .collect::<Vec<_>>();
    let order_template = sequence
        .order_container
        .ok_or_else(|| invalid_field(sequence.path, "an order container"))?;
    let order_container = resolve_template(sequence.path, order_template, context)?;
    let next_is_present = value_at_path(next_parent, relative_path).is_some();
    if next_is_present || sequence.required {
        doc.get_map(KEYED_SEQUENCE_PRESENCE_CONTAINER)
            .insert(order_container.as_str(), true)?;
    } else {
        doc.get_map(KEYED_SEQUENCE_PRESENCE_CONTAINER)
            .delete(order_container.as_str())?;
    }
    if previous_order != next_order {
        write_string_list(&doc.get_list(order_container.as_str()), &next_order)?;
    }
    let previous_by_identity = previous_items.into_iter().collect::<HashMap<_, _>>();
    let prefix = format!("{}.*.", sequence.path);
    let empty_previous = Value::Null;
    for (identity, next_item) in next_items {
        let previous_item = previous_by_identity.get(&identity).copied();
        let mut item_context = context.clone();
        item_context.insert(identity_variable(sequence).to_string(), identity.clone());
        for field in direct_sequence_item_fields(plan, sequence) {
            if matches!(
                field.storage_kind,
                GeneratedCollaborationStorageKind::DerivedIdentity
                    | GeneratedCollaborationStorageKind::DerivedRevision
            ) {
                continue;
            }
            let item_path = field
                .path
                .strip_prefix(&prefix)
                .expect("filtered item field");
            write_field_if_changed(
                doc,
                field,
                previous_item.and_then(|item| value_at_path(item, item_path)),
                value_at_path(next_item, item_path),
                &item_context,
            )?;
        }
        for child in direct_child_sequences(plan, Some(sequence.path)) {
            write_sequence_changes(
                plan,
                doc,
                child,
                previous_item.unwrap_or(&empty_previous),
                next_item,
                &item_context,
            )?;
        }
    }
    Ok(())
}

fn sequence_is_explicitly_present(
    doc: &LoroDoc,
    sequence: &GeneratedCollaborationFieldSpec,
    context: &IdentityContext,
) -> Result<bool, CollaborationLoroError> {
    let order_template = sequence
        .order_container
        .ok_or_else(|| invalid_field(sequence.path, "an order container"))?;
    let order_container = resolve_template(sequence.path, order_template, context)?;
    Ok(doc
        .get_map(KEYED_SEQUENCE_PRESENCE_CONTAINER)
        .get(order_container.as_str())
        .is_some())
}

fn write_field_if_changed(
    doc: &LoroDoc,
    field: &GeneratedCollaborationFieldSpec,
    previous: Option<&Value>,
    next: Option<&Value>,
    context: &IdentityContext,
) -> Result<(), CollaborationLoroError> {
    if previous == next {
        return Ok(());
    }
    if field.required {
        validate_field_value(field, next)?;
    }
    let container = resolve_container(field, context)?;
    match field.storage_kind {
        GeneratedCollaborationStorageKind::Scalar => {
            let key = field
                .key
                .ok_or_else(|| invalid_field(field.path, "a scalar key"))?;
            let map = doc.get_map(container.as_str());
            match field.codec {
                GeneratedCollaborationValueCodec::Integer => {
                    map.insert(
                        key,
                        next.and_then(Value::as_i64)
                            .ok_or_else(|| invalid_field(field.path, "an integer"))?,
                    )?;
                }
                GeneratedCollaborationValueCodec::Number => {
                    map.insert(
                        key,
                        next.and_then(Value::as_f64)
                            .ok_or_else(|| invalid_field(field.path, "a number"))?,
                    )?;
                }
                GeneratedCollaborationValueCodec::OptionalNumber => {
                    match next.and_then(Value::as_f64) {
                        Some(value) => map.insert(key, value)?,
                        None => map.delete(key)?,
                    }
                }
                GeneratedCollaborationValueCodec::Boolean => {
                    map.insert(
                        key,
                        next.and_then(Value::as_bool)
                            .ok_or_else(|| invalid_field(field.path, "a boolean"))?,
                    )?;
                }
                GeneratedCollaborationValueCodec::String => {
                    map.insert(
                        key,
                        next.and_then(Value::as_str)
                            .ok_or_else(|| invalid_field(field.path, "a string"))?,
                    )?;
                }
                GeneratedCollaborationValueCodec::OptionalString => {
                    match next.and_then(Value::as_str) {
                        Some(value) if !value.is_empty() => {
                            map.insert(key, value)?;
                        }
                        _ => {
                            map.delete(key)?;
                        }
                    }
                }
                _ => return Err(invalid_field(field.path, "a scalar-compatible codec")),
            }
        }
        GeneratedCollaborationStorageKind::Text => {
            let text = match field.codec {
                GeneratedCollaborationValueCodec::String => next
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_field(field.path, "a string"))?
                    .to_string(),
                GeneratedCollaborationValueCodec::OptionalString => {
                    next.and_then(Value::as_str).unwrap_or_default().to_string()
                }
                GeneratedCollaborationValueCodec::PropertyText => property_text(field, next)?.1,
                _ => return Err(invalid_field(field.path, "a text-compatible codec")),
            };
            write_text(&doc.get_text(container.as_str()), field.path, &text)?;
            if field.codec == GeneratedCollaborationValueCodec::PropertyText {
                let (mime, _) = property_text(field, next)?;
                let (metadata_container, metadata_key) = resolve_metadata(field, context)?;
                doc.get_map(metadata_container.as_str())
                    .insert(metadata_key, mime)?;
            }
        }
        GeneratedCollaborationStorageKind::OrderedList => {
            write_string_list(
                &doc.get_list(container.as_str()),
                &string_array(next, field.path, field.required)?,
            )?;
        }
        GeneratedCollaborationStorageKind::StructuredList => {
            let values = next
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_field(field.path, "an array"))?
                .iter()
                .map(json_to_loro)
                .collect::<Result<Vec<_>, _>>()?;
            write_list(&doc.get_list(container.as_str()), &values)?;
        }
        GeneratedCollaborationStorageKind::StructuredMap => {
            let values = match next {
                Some(Value::Object(values)) => values,
                None if !field.required => {
                    let map = doc.get_map(container.as_str());
                    let mut keys = Vec::new();
                    map.for_each(|key, _| keys.push(key.to_string()));
                    for key in keys {
                        map.delete(&key)?;
                    }
                    doc.get_map(STRUCTURED_MAP_PRESENCE_CONTAINER)
                        .delete(container.as_str())?;
                    return Ok(());
                }
                _ => return Err(invalid_field(field.path, "an object")),
            };
            let map = doc.get_map(container.as_str());
            let previous_values = previous
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            write_structured_map_changes(&map, &previous_values, values)?;
            doc.get_map(STRUCTURED_MAP_PRESENCE_CONTAINER)
                .insert(container.as_str(), true)?;
        }
        GeneratedCollaborationStorageKind::StructuredDocument => {
            let values = match next {
                Some(Value::Object(values)) => values,
                None if !field.required => {
                    let map = doc.get_map(container.as_str());
                    let mut keys = Vec::new();
                    map.for_each(|key, _| keys.push(key.to_string()));
                    for key in keys {
                        map.delete(&key)?;
                    }
                    doc.get_map(STRUCTURED_MAP_PRESENCE_CONTAINER)
                        .delete(container.as_str())?;
                    return Ok(());
                }
                _ => return Err(invalid_field(field.path, "an object")),
            };
            let map = doc.get_map(container.as_str());
            let previous_values = previous
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            write_structured_document_changes(&map, &previous_values, values)?;
            doc.get_map(STRUCTURED_MAP_PRESENCE_CONTAINER)
                .insert(container.as_str(), true)?;
        }
        GeneratedCollaborationStorageKind::DerivedIdentity
        | GeneratedCollaborationStorageKind::DerivedRevision
        | GeneratedCollaborationStorageKind::KeyedSequence => {}
    }
    Ok(())
}

fn read_field(
    doc: &LoroDoc,
    field: &GeneratedCollaborationFieldSpec,
    context: &IdentityContext,
    mime_source: Option<MimeSource<'_>>,
) -> Result<Option<Value>, CollaborationLoroError> {
    let container = resolve_container(field, context)?;
    Ok(match field.storage_kind {
        GeneratedCollaborationStorageKind::Scalar => {
            let key = field
                .key
                .ok_or_else(|| invalid_field(field.path, "a scalar key"))?;
            doc.get_map(container.as_str())
                .get(key)
                .map(|value| value.get_deep_value().to_json_value())
        }
        GeneratedCollaborationStorageKind::Text => {
            let text = doc.get_text(container.as_str()).to_string();
            if field.codec == GeneratedCollaborationValueCodec::OptionalString && text.is_empty() {
                return Ok(None);
            }
            Some(match field.codec {
                GeneratedCollaborationValueCodec::PropertyText => {
                    let (metadata_container, metadata_key) = resolve_metadata(field, context)?;
                    let stored_mime = doc
                        .get_map(metadata_container.as_str())
                        .get(metadata_key)
                        .map(|value| value.get_deep_value().to_json_value())
                        .and_then(|value| value.as_str().map(str::to_string));
                    let mime = match (stored_mime, mime_source) {
                        (Some(mime), _) => mime,
                        (None, Some(source)) => source_property_mime(source, field, context)?,
                        (None, None) => {
                            return Err(invalid_field(field.path, "a stored MIME value"));
                        }
                    };
                    serde_json::json!({
                        "$mime": mime,
                        "value": text,
                    })
                }
                _ => Value::String(text),
            })
        }
        GeneratedCollaborationStorageKind::OrderedList
        | GeneratedCollaborationStorageKind::StructuredList => Some(
            doc.get_list(container.as_str())
                .get_deep_value()
                .to_json_value(),
        ),
        GeneratedCollaborationStorageKind::StructuredMap => {
            let value = doc
                .get_map(container.as_str())
                .get_deep_value()
                .to_json_value();
            let explicitly_present = doc
                .get_map(STRUCTURED_MAP_PRESENCE_CONTAINER)
                .get(container.as_str())
                .is_some();
            if field.required
                || explicitly_present
                || value.as_object().is_some_and(|values| !values.is_empty())
            {
                Some(value)
            } else {
                None
            }
        }
        GeneratedCollaborationStorageKind::StructuredDocument => {
            let map = doc.get_map(container.as_str());
            let value = read_structured_document(&map)?;
            let explicitly_present = doc
                .get_map(STRUCTURED_MAP_PRESENCE_CONTAINER)
                .get(container.as_str())
                .is_some();
            if field.required
                || explicitly_present
                || value.as_object().is_some_and(|values| !values.is_empty())
            {
                Some(value)
            } else {
                None
            }
        }
        GeneratedCollaborationStorageKind::DerivedIdentity
        | GeneratedCollaborationStorageKind::DerivedRevision
        | GeneratedCollaborationStorageKind::KeyedSequence => None,
    })
}

fn source_property_mime(
    source: MimeSource<'_>,
    field: &GeneratedCollaborationFieldSpec,
    context: &IdentityContext,
) -> Result<String, CollaborationLoroError> {
    let property = if context.is_empty() {
        value_at_path(source.document, field.path)
    } else {
        let (sequence_path, item_path) = field
            .path
            .split_once(".*.")
            .ok_or_else(|| invalid_field(field.path, "a keyed field path"))?;
        let (identity_path, identity_value) = context
            .iter()
            .next()
            .ok_or_else(|| invalid_field(field.path, "a keyed identity value"))?;
        value_at_path(source.document, sequence_path)
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    value_at_path(item, identity_path).and_then(Value::as_str)
                        == Some(identity_value.as_str())
                })
            })
            .and_then(|item| value_at_path(item, item_path))
    };
    Ok(property
        .and_then(|value| value.get("$mime"))
        .and_then(Value::as_str)
        .filter(|mime| mime.starts_with("text/"))
        .unwrap_or(source.default_mime)
        .to_string())
}

fn direct_child_sequences<'a>(
    plan: &'a GeneratedCollaborationEntitySpec,
    parent_path: Option<&str>,
) -> Vec<&'a GeneratedCollaborationFieldSpec> {
    plan.fields
        .iter()
        .filter(|field| {
            if field.storage_kind != GeneratedCollaborationStorageKind::KeyedSequence {
                return false;
            }
            match parent_path {
                None => !field.path.contains(".*."),
                Some(parent) => {
                    let prefix = format!("{parent}.*.");
                    field.path.starts_with(&prefix) && !field.path[prefix.len()..].contains(".*.")
                }
            }
        })
        .collect()
}

fn direct_sequence_item_fields<'a>(
    plan: &'a GeneratedCollaborationEntitySpec,
    sequence: &GeneratedCollaborationFieldSpec,
) -> Vec<&'a GeneratedCollaborationFieldSpec> {
    let prefix = format!("{}.*.", sequence.path);
    plan.fields
        .iter()
        .filter(|field| {
            field.storage_kind != GeneratedCollaborationStorageKind::KeyedSequence
                && field.path.starts_with(&prefix)
                && !field.path[prefix.len()..].contains(".*.")
        })
        .collect()
}

fn parent_sequence_path(
    plan: &GeneratedCollaborationEntitySpec,
    sequence_path: &str,
) -> Option<&'static str> {
    plan.fields
        .iter()
        .filter(|field| {
            field.storage_kind == GeneratedCollaborationStorageKind::KeyedSequence
                && sequence_path.starts_with(&format!("{}.*.", field.path))
        })
        .max_by_key(|field| field.path.len())
        .map(|field| field.path)
}

fn sequence_relative_path(
    plan: &GeneratedCollaborationEntitySpec,
    sequence: &GeneratedCollaborationFieldSpec,
) -> &'static str {
    let Some(parent) = parent_sequence_path(plan, sequence.path) else {
        return sequence.path;
    };
    let offset = parent.len() + ".*.".len();
    &sequence.path[offset..]
}

fn identity_variable(sequence: &GeneratedCollaborationFieldSpec) -> &'static str {
    sequence
        .identity_variable
        .or(sequence.identity_path)
        .expect("validated keyed sequence identity")
}

fn resolve_container(
    field: &GeneratedCollaborationFieldSpec,
    context: &IdentityContext,
) -> Result<String, CollaborationLoroError> {
    let template = field
        .container
        .or(field.container_template)
        .ok_or_else(|| invalid_field(field.path, "a container"))?;
    resolve_template(field.path, template, context)
}

fn resolve_metadata(
    field: &GeneratedCollaborationFieldSpec,
    context: &IdentityContext,
) -> Result<(String, &'static str), CollaborationLoroError> {
    let template = field
        .metadata_container
        .or(field.metadata_container_template)
        .ok_or_else(|| invalid_field(field.path, "a MIME metadata container"))?;
    let key = field
        .metadata_key
        .ok_or_else(|| invalid_field(field.path, "a MIME metadata key"))?;
    let resolved = resolve_template(field.path, template, context)?;
    Ok((resolved, key))
}

fn resolve_template(
    path: &str,
    template: &str,
    context: &IdentityContext,
) -> Result<String, CollaborationLoroError> {
    let mut resolved = template.to_string();
    for (name, value) in context {
        resolved = resolved.replace(&format!("{{{name}}}"), value);
    }
    if resolved.contains('{') {
        return Err(invalid_field(path, "resolved container identities"));
    }
    Ok(resolved)
}

fn validate_field_value(
    field: &GeneratedCollaborationFieldSpec,
    value: Option<&Value>,
) -> Result<(), CollaborationLoroError> {
    match field.codec {
        GeneratedCollaborationValueCodec::Integer if value.and_then(Value::as_i64).is_none() => {
            Err(invalid_field(field.path, "an integer"))
        }
        GeneratedCollaborationValueCodec::Number
        | GeneratedCollaborationValueCodec::OptionalNumber
            if value.and_then(Value::as_f64).is_none() =>
        {
            Err(invalid_field(field.path, "a number"))
        }
        GeneratedCollaborationValueCodec::Boolean if value.and_then(Value::as_bool).is_none() => {
            Err(invalid_field(field.path, "a boolean"))
        }
        GeneratedCollaborationValueCodec::String
        | GeneratedCollaborationValueCodec::OptionalString
        | GeneratedCollaborationValueCodec::Identity
            if value.and_then(Value::as_str).is_none() =>
        {
            Err(invalid_field(field.path, "a string"))
        }
        GeneratedCollaborationValueCodec::PropertyText => property_text(field, value).map(|_| ()),
        GeneratedCollaborationValueCodec::StringList
            if string_array(value, field.path, field.required).is_err() =>
        {
            Err(invalid_field(field.path, "an array of strings"))
        }
        GeneratedCollaborationValueCodec::StructuredJson
            if is_object_storage_kind(field.storage_kind)
                && value.and_then(Value::as_object).is_none() =>
        {
            Err(invalid_field(field.path, "an object"))
        }
        GeneratedCollaborationValueCodec::StructuredJson
        | GeneratedCollaborationValueCodec::KeyedSequence
            if !is_object_storage_kind(field.storage_kind)
                && value.and_then(Value::as_array).is_none() =>
        {
            Err(invalid_field(field.path, "an array"))
        }
        _ => Ok(()),
    }
}

fn keyed_items<'a>(
    document: &'a Value,
    path: &str,
    identity_path: &str,
) -> Result<Vec<(String, &'a Value)>, CollaborationLoroError> {
    value_at_path(document, path)
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(|item| {
            let identity = value_at_path(item, identity_path)
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_field(identity_path, "a string"))?;
            Ok((identity.to_string(), item))
        })
        .collect()
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(value, |current, segment| current.get(segment))
}

fn set_value_at_path(
    root: &mut Value,
    path: &str,
    value: Value,
) -> Result<(), CollaborationLoroError> {
    let segments = path.split('.').collect::<Vec<_>>();
    let Some((last, parents)) = segments.split_last() else {
        return Err(invalid_field(path, "a non-empty path"));
    };
    let mut current = root;
    for segment in parents {
        let object = current
            .as_object_mut()
            .ok_or_else(|| invalid_field(path, "an object path"))?;
        current = object
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    current
        .as_object_mut()
        .ok_or_else(|| invalid_field(path, "an object path"))?
        .insert((*last).to_string(), value);
    Ok(())
}

fn property_text(
    field: &GeneratedCollaborationFieldSpec,
    value: Option<&Value>,
) -> Result<(String, String), CollaborationLoroError> {
    let value = value.ok_or_else(|| invalid_field(field.path, "a text property value"))?;
    let mime = value
        .get("$mime")
        .and_then(Value::as_str)
        .filter(|mime| mime.starts_with("text/"))
        .ok_or_else(|| invalid_field(field.path, "a text property MIME"))?;
    let text = value
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_field(field.path, "a text property string value"))?;
    Ok((mime.to_string(), text.to_string()))
}

fn string_array(
    value: Option<&Value>,
    path: &str,
    required: bool,
) -> Result<Vec<String>, CollaborationLoroError> {
    if value.is_none() && !required {
        return Ok(Vec::new());
    }
    value
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_field(path, "an array of strings"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| invalid_field(path, "an array of strings"))
        })
        .collect()
}

fn unique_loro_string_list(list: &LoroList) -> Vec<String> {
    let mut seen = HashSet::new();
    match list.get_deep_value() {
        LoroValue::List(values) => values
            .iter()
            .filter_map(|value| match value {
                LoroValue::String(value) => Some((**value).clone()),
                LoroValue::I64(value) => Some(value.to_string()),
                _ => None,
            })
            .filter(|value| !value.is_empty() && seen.insert(value.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Replaces every character of `text` with `value`, so the replacement
/// consumes exactly the characters the caller captured.
pub(crate) fn replace_all_text(text: &loro::LoroText, value: &str) -> Result<(), loro::LoroError> {
    let length = text.len_utf8();
    if length > 0 {
        text.delete_utf8(0, length)?;
    }
    if !value.is_empty() {
        text.insert_utf8(0, value)?;
    }
    Ok(())
}

/// Rewrites `text` to `value` as the character edits between them, so an
/// unchanged run keeps its identity and a concurrent edit to it merges.
fn write_text(
    text: &loro::LoroText,
    path: &str,
    value: &str,
) -> Result<(), CollaborationLoroError> {
    text.update(value, loro::UpdateOptions::default())
        .map_err(|_| invalid_field(path, "a text update within the time budget"))
}

fn write_string_list(list: &LoroList, values: &[String]) -> Result<(), loro::LoroError> {
    write_list(
        list,
        &values
            .iter()
            .map(|value| LoroValue::String(value.clone().into()))
            .collect::<Vec<_>>(),
    )
}

/// Rewrites `list` to `desired` as the deletions and insertions of a shortest
/// edit script, so an entry both versions hold keeps its identity and an entry
/// another writer inserts concurrently survives the merge.
fn write_list(list: &LoroList, desired: &[LoroValue]) -> Result<(), loro::LoroError> {
    let current = match list.get_deep_value() {
        LoroValue::List(values) => values.iter().cloned().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let mut edits = Capture::new();
    let Ok(()) = myers::diff(
        &mut edits,
        current.as_slice(),
        0..current.len(),
        desired,
        0..desired.len(),
    );
    let mut position = 0;
    for edit in edits.into_ops() {
        let (deleted, inserted) = match edit {
            DiffOp::Equal { len, .. } => {
                position += len;
                continue;
            }
            DiffOp::Delete { old_len, .. } => (old_len, 0..0),
            DiffOp::Insert {
                new_index, new_len, ..
            } => (0, new_index..new_index + new_len),
            DiffOp::Replace {
                old_len,
                new_index,
                new_len,
                ..
            } => (old_len, new_index..new_index + new_len),
        };
        if deleted > 0 {
            list.delete(position, deleted)?;
        }
        for value in &desired[inserted] {
            list.insert(position, value.clone())?;
            position += 1;
        }
    }
    Ok(())
}

fn write_structured_map_changes(
    map: &LoroMap,
    previous: &Map<String, Value>,
    next: &Map<String, Value>,
) -> Result<(), CollaborationLoroError> {
    for key in previous.keys().filter(|key| !next.contains_key(*key)) {
        map.delete(key)?;
    }

    for (key, next_value) in next {
        let previous_value = previous.get(key);
        if previous_value == Some(next_value) {
            continue;
        }
        if let Value::Object(next_values) = next_value {
            let child = ensure_structured_map_child(map, key)?;
            let previous_values = previous_value
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            write_structured_map_changes(&child, &previous_values, next_values)?;
        } else {
            map.insert(key, json_to_loro(next_value)?)?;
        }
    }
    Ok(())
}

/// Reserved keys describing an identity-keyed list inside a structured
/// document. A plain object carrying `$keyedBy` is stored as an opaque value
/// instead, so the reader can never mistake authored data for this shape.
const KEYED_LIST_MARKER: &str = "$keyedBy";
const KEYED_LIST_ORDER: &str = "$order";
const KEYED_LIST_ITEMS: &str = "$items";
const KEYED_LIST_IDENTITIES: [&str; 2] = ["id", "uid"];

fn is_object_storage_kind(kind: GeneratedCollaborationStorageKind) -> bool {
    matches!(
        kind,
        GeneratedCollaborationStorageKind::StructuredMap
            | GeneratedCollaborationStorageKind::StructuredDocument
    )
}

/// The identity key a list of objects can be keyed by, when every element
/// carries it as a unique non-empty string.
fn keyed_list_identity(values: &[Value]) -> Option<&'static str> {
    if values.is_empty() {
        return None;
    }
    KEYED_LIST_IDENTITIES.into_iter().find(|identity| {
        let mut seen = std::collections::HashSet::new();
        values.iter().all(|value| {
            value
                .as_object()
                .filter(|item| !item.contains_key(KEYED_LIST_MARKER))
                .and_then(|item| item.get(*identity))
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty())
                .is_some_and(|key| seen.insert(key.to_string()))
        })
    })
}

/// Writes one structured document level.
///
/// Strings become text containers so concurrent typing merges by character;
/// lists whose elements carry a stable identity become keyed so two writers
/// adding entries both keep theirs. Everything else stays a plain value, which
/// is last-writer-wins.
fn write_structured_document_changes(
    map: &LoroMap,
    previous: &Map<String, Value>,
    next: &Map<String, Value>,
) -> Result<(), CollaborationLoroError> {
    for key in previous.keys().filter(|key| !next.contains_key(*key)) {
        map.delete(key)?;
    }

    for (key, next_value) in next {
        let previous_value = previous.get(key);
        if previous_value == Some(next_value) {
            continue;
        }
        match next_value {
            Value::String(text) => {
                let container = ensure_structured_document_text(map, key)?;
                // A diff-based update, so a concurrent edit to the same
                // prose merges instead of replacing the other writer's text.
                container
                    .update(text, loro::UpdateOptions::default())
                    .map_err(|_| invalid_field(key, "a text update within the time budget"))?;
            }
            Value::Object(next_values) if !next_values.contains_key(KEYED_LIST_MARKER) => {
                let child = ensure_structured_map_child(map, key)?;
                let previous_values = previous_value
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                write_structured_document_changes(&child, &previous_values, next_values)?;
            }
            Value::Array(values) => match keyed_list_identity(values) {
                Some(identity) => {
                    let child = ensure_structured_map_child(map, key)?;
                    write_keyed_list_changes(
                        &child,
                        identity,
                        previous_value
                            .and_then(Value::as_array)
                            .map(Vec::as_slice)
                            .unwrap_or_default(),
                        values,
                    )?;
                }
                None => {
                    map.insert(key, json_to_loro(next_value)?)?;
                }
            },
            _ => {
                map.insert(key, json_to_loro(next_value)?)?;
            }
        }
    }
    Ok(())
}

fn write_keyed_list_changes(
    map: &LoroMap,
    identity: &str,
    previous: &[Value],
    next: &[Value],
) -> Result<(), CollaborationLoroError> {
    map.insert(KEYED_LIST_MARKER, identity)?;
    let items = ensure_structured_map_child(map, KEYED_LIST_ITEMS)?;
    let previous_by_identity = list_by_identity(previous, identity);
    let next_by_identity = list_by_identity(next, identity);

    for key in previous_by_identity
        .keys()
        .filter(|key| !next_by_identity.contains_key(*key))
    {
        items.delete(key)?;
    }
    for (key, value) in &next_by_identity {
        let child = ensure_structured_map_child(&items, key)?;
        let previous_values = previous_by_identity
            .get(key)
            .and_then(|value| value.as_object())
            .cloned()
            .unwrap_or_default();
        let next_values = value
            .as_object()
            .cloned()
            .ok_or_else(|| invalid_field(key, "an object"))?;
        write_structured_document_changes(&child, &previous_values, &next_values)?;
    }

    write_string_list(
        &map.ensure_mergeable_list(KEYED_LIST_ORDER)?,
        &next
            .iter()
            .filter_map(|value| {
                value
                    .get(identity)
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

fn list_by_identity(values: &[Value], identity: &str) -> Map<String, Value> {
    values
        .iter()
        .filter_map(|value| {
            value
                .get(identity)
                .and_then(Value::as_str)
                .map(|key| (key.to_string(), value.clone()))
        })
        .collect()
}

fn ensure_structured_document_text(
    map: &LoroMap,
    key: &str,
) -> Result<loro::LoroText, CollaborationLoroError> {
    if matches!(
        map.get(key),
        Some(ValueOrContainer::Container(Container::Text(_))) | None
    ) {
        return Ok(map.ensure_mergeable_text(key)?);
    }

    map.delete(key)?;
    Ok(map.ensure_mergeable_text(key)?)
}

/// Rebuilds one structured document level from its containers.
fn read_structured_document(map: &LoroMap) -> Result<Value, CollaborationLoroError> {
    let mut keys = Vec::new();
    map.for_each(|key, _| keys.push(key.to_string()));
    let mut values = Map::new();
    for key in keys {
        let Some(entry) = map.get(&key) else {
            continue;
        };
        let value = match entry {
            ValueOrContainer::Container(Container::Text(text)) => Value::String(text.to_string()),
            ValueOrContainer::Container(Container::Map(child)) => {
                if child.get(KEYED_LIST_MARKER).is_some() {
                    read_keyed_list(&child)?
                } else {
                    read_structured_document(&child)?
                }
            }
            ValueOrContainer::Container(Container::List(list)) => {
                list.get_deep_value().to_json_value()
            }
            ValueOrContainer::Container(Container::MovableList(list)) => {
                list.get_deep_value().to_json_value()
            }
            ValueOrContainer::Container(_) => Value::Null,
            ValueOrContainer::Value(value) => value.to_json_value(),
        };
        values.insert(key, value);
    }
    Ok(Value::Object(values))
}

fn read_keyed_list(map: &LoroMap) -> Result<Value, CollaborationLoroError> {
    let identity = map
        .get(KEYED_LIST_MARKER)
        .map(|value| value.get_deep_value().to_json_value())
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| invalid_field(KEYED_LIST_MARKER, "a stored identity key"))?;
    let items = match map.get(KEYED_LIST_ITEMS) {
        Some(ValueOrContainer::Container(Container::Map(items))) => items,
        _ => return Ok(Value::Array(Vec::new())),
    };
    let mut item_keys = Vec::new();
    items.for_each(|key, _| item_keys.push(key.to_string()));

    let order = match map.get(KEYED_LIST_ORDER) {
        Some(ValueOrContainer::Container(Container::List(order))) => order
            .get_deep_value()
            .to_json_value()
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    // Order first, then anything an order rewrite lost: a concurrently added
    // item keeps its place in the document even when the two order lists
    // disagreed.
    let mut seen = std::collections::HashSet::new();
    let mut ordered = Vec::new();
    for key in order.into_iter().chain({
        item_keys.sort();
        item_keys.into_iter()
    }) {
        if !seen.insert(key.clone()) {
            continue;
        }
        let Some(ValueOrContainer::Container(Container::Map(child))) = items.get(&key) else {
            continue;
        };
        let mut value = read_structured_document(&child)?;
        if let Some(object) = value.as_object_mut() {
            object.insert(identity.clone(), Value::String(key));
        }
        ordered.push(value);
    }
    Ok(Value::Array(ordered))
}

fn ensure_structured_map_child(
    map: &LoroMap,
    key: &str,
) -> Result<LoroMap, CollaborationLoroError> {
    if matches!(
        map.get(key),
        Some(ValueOrContainer::Container(Container::Map(_))) | None
    ) {
        return Ok(map.ensure_mergeable_map(key)?);
    }

    map.delete(key)?;
    Ok(map.ensure_mergeable_map(key)?)
}

pub(crate) fn json_to_loro(value: &Value) -> Result<LoroValue, CollaborationLoroError> {
    Ok(match value {
        Value::Null => LoroValue::Null,
        Value::Bool(value) => LoroValue::Bool(*value),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                LoroValue::I64(integer)
            } else {
                LoroValue::Double(
                    value
                        .as_f64()
                        .ok_or_else(|| invalid_field("$", "a finite number"))?,
                )
            }
        }
        Value::String(value) => LoroValue::String(value.clone().into()),
        Value::Array(values) => LoroValue::List(
            values
                .iter()
                .map(json_to_loro)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
        Value::Object(values) => LoroValue::Map(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_loro(value)?)))
                .collect::<Result<HashMap<_, _>, CollaborationLoroError>>()?
                .into(),
        ),
    })
}

fn invalid_field(path: &str, expected: &'static str) -> CollaborationLoroError {
    CollaborationLoroError::InvalidField {
        path: path.to_string(),
        expected,
    }
}

//! Reading the schema subset: local references, type unions, nullable schemas,
//! tagged unions, and the string arrays schema nodes carry.

use crate::definition::Definition;
use crate::error::{refuse, Result};
use crate::json::{string_literal, Json, Object};
use crate::names::pascal_identifier;

const PRIMITIVE_UNION_TYPES: [&str; 5] = ["string", "boolean", "integer", "number", "null"];

/// Types a `type` union may name only beside `null`.
const NULLABLE_ONLY_TYPES: [&str; 2] = ["array", "object"];

/// The object at `key`, if it is one.
pub(crate) fn object_at<'a>(schema: &'a Object, key: &str) -> Option<&'a Object> {
    schema.get(key).and_then(Json::as_object)
}

/// Whether `schema.type` is the string `expected`.
pub(crate) fn has_type(schema: &Object, expected: &str) -> bool {
    schema.get("type").and_then(Json::as_str) == Some(expected)
}

/// The values schema of a record: `additionalProperties` of an object schema
/// that declares no `properties`.
pub(crate) fn record_values_schema(schema: &Object) -> Option<&Object> {
    if !has_type(schema, "object") || schema.get("properties").is_some() {
        return None;
    }
    object_at(schema, "additionalProperties")
}

/// The schema of an open object's other members: `additionalProperties` of an
/// object schema that declares `properties` beside it.
pub(crate) fn other_members_schema(schema: &Object) -> Option<&Object> {
    if !has_type(schema, "object") {
        return None;
    }
    object_at(schema, "properties")?;
    object_at(schema, "additionalProperties")
}

/// Whether `value` meets the `minLength`, `minimum` and `maximum` of
/// `schema` that apply to its type.
pub(crate) fn value_meets_bounds(schema: &Object, value: &Json) -> bool {
    let bound = |keyword: &str| schema.get(keyword).and_then(Json::as_f64);
    if let Some(text) = value.as_str() {
        return bound("minLength").is_none_or(|limit| text.chars().count() as f64 >= limit);
    }
    let Some(number) = value.as_f64() else {
        return true;
    };
    bound("minimum").is_none_or(|limit| number >= limit)
        && bound("maximum").is_none_or(|limit| number <= limit)
}

/// The member types of a type union (`"type": ["string", "integer"]`), or
/// `None` when `type` is not an array. A union combines scalar types and
/// `null`, or pairs an array or object type with `null`.
pub(crate) fn primitive_union_types(schema: &Object) -> Result<Option<Vec<&str>>> {
    let Some(Json::Array(types)) = schema.get("type") else {
        return Ok(None);
    };
    let members: Option<Vec<&str>> = types
        .iter()
        .map(|entry| {
            entry.as_str().filter(|name| {
                PRIMITIVE_UNION_TYPES.contains(name) || NULLABLE_ONLY_TYPES.contains(name)
            })
        })
        .collect();
    let composite = |members: &[&str]| {
        members
            .iter()
            .any(|member| NULLABLE_ONLY_TYPES.contains(member))
    };
    let Some(members) = members.filter(|members| {
        members.len() >= 2
            && (!composite(members) || (members.len() == 2 && members.contains(&"null")))
    }) else {
        return refuse(format!(
            "Type unions may only combine {}, or pair {} with null.",
            PRIMITIVE_UNION_TYPES.join(", "),
            NULLABLE_ONLY_TYPES.join(" or ")
        ));
    };
    let mut unique = members.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != members.len() {
        return refuse("Type unions may not repeat a type.");
    }
    Ok(Some(members))
}

/// Whether `schema` is `{"type": "null"}`, with annotations at most.
pub(crate) fn is_null_schema(schema: &Json) -> bool {
    schema.as_object().is_some_and(|schema| {
        has_type(schema, "null")
            && schema
                .keys()
                .all(|key| matches!(key, "type" | "description" | "title"))
    })
}

/// The schema a nullable schema holds beside `null`: its `type` union without
/// `null`, or the member of an `anyOf` that pairs a schema with
/// `{"type": "null"}`. `None` for a schema that does not admit `null`.
pub(crate) fn nullable_inner(schema: &Object) -> Option<Object> {
    if let Some(members) = schema.get("anyOf").and_then(Json::as_array) {
        let [first, second] = members else {
            return None;
        };
        let inner = match (is_null_schema(first), is_null_schema(second)) {
            (true, false) => second,
            (false, true) => first,
            _ => return None,
        };
        return inner.as_object().cloned();
    }
    let Some(Json::Array(types)) = schema.get("type") else {
        return None;
    };
    if !types.iter().any(|member| member.as_str() == Some("null")) {
        return None;
    }
    let others: Vec<Json> = types
        .iter()
        .filter(|member| member.as_str() != Some("null"))
        .cloned()
        .collect();
    // A `null` default is a default of the nullable schema, not of its inner one.
    let mut inner: Object = schema
        .iter()
        .filter(|(key, value)| !(*key == "default" && **value == Json::Null))
        .map(|(key, value)| (key, value.clone()))
        .collect();
    inner.insert(
        "type",
        match others.as_slice() {
            [single] => single.clone(),
            _ => Json::Array(others),
        },
    );
    Some(inner)
}

/// Whether `schema` is an externally tagged union: a `oneOf` map, without a
/// `discriminator` or a `type`, whose keys are the tags its data variants are
/// written under and whose `enum` lists its unit variants.
pub(crate) fn is_externally_tagged(schema: &Object) -> bool {
    object_at(schema, "oneOf").is_some()
        && schema.get("discriminator").is_none()
        && schema.get("type").is_none()
}

/// The definition name in a `#/$defs/Name` reference.
pub(crate) fn local_reference_name(reference: Option<&Json>) -> Option<&str> {
    let name = reference?.as_str()?.strip_prefix("#/$defs/")?;
    let mut characters = name.chars();
    let valid = characters.next().is_some_and(|c| c.is_ascii_alphabetic())
        && characters.all(|c| c.is_ascii_alphanumeric());
    valid.then_some(name)
}

/// Resolves a local `$defs` reference to the definition name it names.
pub(crate) fn schema_ref_name(
    definition: &Definition,
    reference: &Object,
    context: &str,
) -> Result<String> {
    let Some(name) = local_reference_name(reference.get("$ref")) else {
        return refuse(format!("{context} must be a local #/$defs reference."));
    };
    if definition.def(name).is_none() {
        return refuse(format!(
            "{context} references unknown definition {}.",
            string_literal(name)
        ));
    }
    Ok(name.to_owned())
}

/// The definition `reference` names, resolved.
pub(crate) fn resolve_ref<'d>(
    definition: &'d Definition,
    reference: &Object,
    context: &str,
) -> Result<&'d Object> {
    let name = schema_ref_name(definition, reference, context)?;
    Ok(definition
        .def(&name)
        .expect("schema_ref_name checked it exists"))
}

/// The generated type name a local `$defs` reference renders as.
pub(crate) fn schema_ref_type_name(reference: &Object) -> Result<String> {
    match local_reference_name(reference.get("$ref")) {
        Some(name) => Ok(pascal_identifier(name)),
        None => refuse(format!(
            "Unsupported schema ref {}.",
            reference
                .get("$ref")
                .map_or_else(|| "undefined".to_owned(), Json::stringify)
        )),
    }
}

/// The `$ref` string of a reference object the meta-schema has shaped.
pub(crate) fn ref_string(reference: &Object) -> &str {
    reference
        .get("$ref")
        .and_then(Json::as_str)
        .expect("the meta-schema requires a $ref string")
}

/// A string array, or an empty one when `value` is absent.
pub(crate) fn read_string_array<'a>(
    value: Option<&'a Json>,
    context: &str,
) -> Result<Vec<&'a str>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let strings: Option<Vec<&str>> = value
        .as_array()
        .and_then(|items| items.iter().map(Json::as_str).collect());
    match strings {
        Some(strings) => Ok(strings),
        None => refuse(format!("{context} must be a string array.")),
    }
}

/// `JSON.stringify(value)` where `value` may be `undefined`.
pub(crate) fn stringify_or_undefined(value: Option<&Json>) -> String {
    value.map_or_else(|| "undefined".to_owned(), Json::stringify)
}

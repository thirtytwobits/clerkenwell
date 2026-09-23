//! Reading the schema subset: local references, primitive unions, and the
//! string arrays schema nodes carry.

use crate::definition::Definition;
use crate::error::{refuse, Result};
use crate::json::{string_literal, Json, Object};
use crate::names::pascal_identifier;

const PRIMITIVE_UNION_TYPES: [&str; 4] = ["string", "boolean", "integer", "number"];

/// The object at `key`, if it is one.
pub(crate) fn object_at<'a>(schema: &'a Object, key: &str) -> Option<&'a Object> {
    schema.get(key).and_then(Json::as_object)
}

/// Whether `schema.type` is the string `expected`.
pub(crate) fn has_type(schema: &Object, expected: &str) -> bool {
    schema.get("type").and_then(Json::as_str) == Some(expected)
}

/// The member types of a primitive union (`"type": ["string", "integer"]`), or
/// `None` when `type` is not an array.
pub(crate) fn primitive_union_types(schema: &Object) -> Result<Option<Vec<&str>>> {
    let Some(Json::Array(types)) = schema.get("type") else {
        return Ok(None);
    };
    let members: Option<Vec<&str>> = types
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .filter(|name| PRIMITIVE_UNION_TYPES.contains(name))
        })
        .collect();
    let Some(members) = members.filter(|members| members.len() >= 2) else {
        return refuse(format!(
            "Primitive unions may only combine {}.",
            PRIMITIVE_UNION_TYPES.join(", ")
        ));
    };
    let mut unique = members.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != members.len() {
        return refuse("Primitive unions may not repeat a type.");
    }
    Ok(Some(members))
}

/// The definition name in a `#/$defs/Name` reference.
fn local_reference_name(reference: Option<&Json>) -> Option<&str> {
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

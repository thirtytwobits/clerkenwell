//! Compose JSON Schema bundles into the generator's schema subset.
//!
//! A projection definition imports, from the bundles its configuration names,
//! the definitions it reaches: those its own sections and `$defs` reference,
//! and those they reference in turn. Each is converted to the subset's form of
//! the same values, or refused where the subset holds no such form.
//! `docs/schema-types.md` lists the conversions.

use std::collections::HashSet;
use std::path::PathBuf;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use crate::error::{refuse, Error, Result};
use crate::json::Json;

/// Keywords JSON Schema defines as annotations only: they describe values
/// without constraining them, and the subset carries no form of them.
const DROPPED_ANNOTATIONS: [&str; 8] = [
    "$comment",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
    "deprecated",
    "examples",
    "readOnly",
    "writeOnly",
];

/// Keywords a converted schema carries as they are.
const KEPT: [&str; 6] = [
    "description",
    "title",
    "default",
    "minLength",
    "minimum",
    "maximum",
];

/// The largest finite `f32`, the bound of a `float` number.
const FLOAT_LIMIT: f64 = f32::MAX as f64;

pub(crate) fn compose(mut document: Json, paths: &[PathBuf]) -> Result<Json> {
    if paths.is_empty() {
        return Ok(document);
    }
    let roots = {
        let mut names = Vec::new();
        json_references(&document, &mut names);
        names
    };
    let Some(definitions) = document
        .as_object_mut()
        .and_then(|document| document.get_mut("$defs"))
        .and_then(Json::as_object_mut)
    else {
        return refuse("A projection definition importing schemas must declare $defs.");
    };
    let mut imports: IndexMap<String, (Value, String)> = IndexMap::new();
    for path in paths {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        let bundle: Value = serde_json::from_str(&text).map_err(|error| Error::Parse {
            path: path.clone(),
            message: error.to_string(),
        })?;
        let Some(bundle_definitions) = bundle.get("$defs").and_then(Value::as_object) else {
            return refuse(format!(
                "Schema import {} must declare a $defs object.",
                path.display()
            ));
        };
        for (name, schema) in bundle_definitions {
            if definitions.contains_key(name) || imports.contains_key(name) {
                return refuse(format!(
                    "Schema import {} conflicts with existing definition {name}.",
                    path.display()
                ));
            }
            imports.insert(
                name.clone(),
                (schema.clone(), format!("{}.$defs.{name}", path.display())),
            );
        }
    }

    // The definitions reached from the projection definition, through the
    // imported definitions they reach.
    let mut converted: IndexMap<&str, Value> = IndexMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending = roots;
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some((imported_name, (schema, context))) = imports.get_key_value(&name) else {
            continue;
        };
        let schema = convert(schema, context)?;
        value_references(&schema, &mut pending);
        converted.insert(imported_name, schema);
    }
    for (name, _) in imports.iter() {
        if let Some(schema) = converted.get(name.as_str()) {
            definitions.insert(
                name,
                Json::parse(&schema.to_string())
                    .map_err(|error| Error::Definition(error.to_string()))?,
            );
        }
    }
    Ok(document)
}

/// The `#/$defs` names `value` references, at any depth.
fn json_references(value: &Json, names: &mut Vec<String>) {
    match value {
        Json::Object(object) => {
            for (key, member) in object.iter() {
                match (key, member) {
                    ("$ref", Json::String(reference)) => {
                        if let Some(name) = reference.strip_prefix("#/$defs/") {
                            names.push(name.to_owned());
                        }
                    }
                    _ => json_references(member, names),
                }
            }
        }
        Json::Array(items) => items.iter().for_each(|item| json_references(item, names)),
        _ => {}
    }
}

/// The `#/$defs` names `value` references, at any depth.
fn value_references(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, member) in object {
                match (key.as_str(), member) {
                    ("$ref", Value::String(reference)) => {
                        if let Some(name) = reference.strip_prefix("#/$defs/") {
                            names.push(name.to_owned());
                        }
                    }
                    _ => value_references(member, names),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| value_references(item, names)),
        _ => {}
    }
}

fn convert(schema: &Value, context: &str) -> Result<Value> {
    let source = match schema {
        Value::Bool(true) => {
            return Ok(
                json!({"type": "json", "description": "An opaque JSON value declared by the imported contract."}),
            )
        }
        Value::Object(source) => source,
        _ => return refuse(format!("{context} requires a representable object schema.")),
    };
    let source: Map<String, Value> = source
        .iter()
        .filter(|(key, _)| !DROPPED_ANNOTATIONS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if source
        .keys()
        .all(|key| matches!(key.as_str(), "description" | "title"))
    {
        // A schema with no constraint admits any JSON value.
        let mut opaque = source;
        opaque.insert("type".into(), "json".into());
        return Ok(Value::Object(opaque));
    }
    if source.contains_key("$ref") {
        if let Some(key) = source
            .keys()
            .find(|key| !["$ref", "default", "description", "title"].contains(&key.as_str()))
        {
            return refuse(format!("{context} cannot combine $ref with {key}."));
        }
        return Ok(Value::Object(source));
    }
    if let Some(members) = source.get("anyOf") {
        return any_of(&source, members, context);
    }
    if let Some(variants) = source.get("oneOf") {
        return one_of(&source, variants, context);
    }
    typed(source, context)
}

/// A schema with a type, written or implied by its keywords.
fn typed(mut source: Map<String, Value>, context: &str) -> Result<Value> {
    if !source.contains_key("type") {
        let implied = if let Some(constant) = source.get("const") {
            match constant {
                Value::String(_) => "string",
                Value::Bool(_) => "boolean",
                Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
                Value::Number(_) => "number",
                Value::Null => "null",
                _ => return refuse(format!("{context}.const must be a scalar.")),
            }
        } else if source
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().all(Value::is_string))
        {
            "string"
        } else if source.contains_key("properties") || source.contains_key("additionalProperties") {
            "object"
        } else if source.contains_key("items") {
            "array"
        } else {
            return refuse(format!(
                "{context} declares no type and no keyword that implies one."
            ));
        };
        source.insert("type".into(), implied.into());
    }
    let types: Vec<String> = match source.get("type") {
        Some(Value::String(single)) => vec![single.clone()],
        Some(Value::Array(members)) => {
            match members
                .iter()
                .map(|member| member.as_str().map(str::to_owned))
                .collect()
            {
                Some(members) => members,
                None => return refuse(format!("{context}.type must name types.")),
            }
        }
        _ => return refuse(format!("{context}.type must name types.")),
    };
    let mut result = Map::new();
    for (key, value) in &source {
        match key.as_str() {
            "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "format"
            | "enum"
            | "const"
            | "exclusiveMinimum"
            | "exclusiveMaximum" => {}
            key if KEPT.contains(&key) => {
                result.insert(key.to_owned(), value.clone());
            }
            key => {
                return refuse(format!(
                    "{context} uses unsupported imported schema keyword {key}."
                ))
            }
        }
    }
    result.insert("type".into(), source["type"].clone());
    if let Some(values) = source.get("enum") {
        let Some(values) = values
            .as_array()
            .filter(|values| !values.is_empty() && values.iter().all(Value::is_string))
        else {
            return refuse(format!("{context}.enum must list strings."));
        };
        result.insert("enum".into(), Value::Array(values.clone()));
    }
    if let Some(constant) = source.get("const") {
        result.insert("const".into(), constant.clone());
    }
    let integral = types.iter().any(|member| member == "integer")
        && !types.iter().any(|member| member == "number");
    let numeric = types
        .iter()
        .any(|member| matches!(member.as_str(), "integer" | "number"));
    for (keyword, bound, step) in [
        ("exclusiveMinimum", "minimum", 1.0),
        ("exclusiveMaximum", "maximum", -1.0),
    ] {
        let Some(limit) = source.get(keyword) else {
            continue;
        };
        let Some(limit) = limit.as_f64().filter(|_| integral) else {
            return refuse(format!(
                "{context}.{keyword} is representable only as an integer bound."
            ));
        };
        // The integers past an exclusive bound start at the next integer.
        let inclusive = if step > 0.0 {
            limit.floor() + step
        } else {
            limit.ceil() + step
        };
        tighten(&mut result, bound, inclusive, context)?;
    }
    if let Some(format) = source.get("format").and_then(Value::as_str) {
        // A format on other values is an annotation, which the subset does not carry.
        if numeric {
            numeric_format(&mut result, format, integral, context)?;
        }
    }
    if types.iter().any(|member| member == "object") {
        object(&source, &mut result, context)?;
    } else if let Some(key) = ["properties", "required", "additionalProperties"]
        .into_iter()
        .find(|key| source.contains_key(*key))
    {
        return refuse(format!("{context}.{key} applies only to object schemas."));
    }
    if let Some(items) = source.get("items") {
        result.insert("items".into(), convert(items, &format!("{context}.items"))?);
    }
    Ok(Value::Object(result))
}

/// The members and other members of an object. An object that does not
/// declare `additionalProperties` admits other members of any value.
fn object(
    source: &Map<String, Value>,
    result: &mut Map<String, Value>,
    context: &str,
) -> Result<()> {
    let properties = match source.get("properties") {
        None => Map::new(),
        Some(Value::Object(properties)) => properties.clone(),
        Some(_) => return refuse(format!("{context}.properties must be an object.")),
    };
    let mut fields = Map::new();
    for (name, schema) in &properties {
        fields.insert(
            name.clone(),
            convert(schema, &format!("{context}.properties.{name}"))?,
        );
    }
    let additional = match source.get("additionalProperties") {
        None | Some(Value::Bool(true)) => Some(json!({"type": "json"})),
        Some(Value::Bool(false)) => None,
        Some(schema) => Some(convert(schema, &format!("{context}.additionalProperties"))?),
    };
    match (fields.is_empty(), additional) {
        // A closed object with no members is the empty object.
        (true, None) => {
            result.insert("properties".into(), Value::Object(fields));
            result.insert("additionalProperties".into(), false.into());
        }
        // An object of other members only is a record.
        (true, Some(values)) => {
            if source.contains_key("required") {
                return refuse(format!(
                    "{context}.required names members an object of other members only does not declare."
                ));
            }
            result.insert("additionalProperties".into(), values);
        }
        (false, additional) => {
            result.insert("properties".into(), Value::Object(fields));
            if let Some(required) = source.get("required") {
                result.insert("required".into(), required.clone());
            }
            result.insert(
                "additionalProperties".into(),
                additional.unwrap_or(Value::Bool(false)),
            );
        }
    }
    Ok(())
}

/// Narrows `bound` to `limit` where `limit` is the tighter.
fn tighten(result: &mut Map<String, Value>, bound: &str, limit: f64, context: &str) -> Result<()> {
    let tighter = bound == "minimum";
    if let Some(existing) = result.get(bound) {
        let Some(existing) = existing.as_f64() else {
            return refuse(format!("{context}.{bound} must be a number."));
        };
        if (tighter && existing >= limit) || (!tighter && existing <= limit) {
            return Ok(());
        }
    }
    let limit = if limit.fract() == 0.0 && limit.abs() < 9_007_199_254_740_992.0 {
        json!(limit as i64)
    } else {
        json!(limit)
    };
    result.insert(bound.to_owned(), limit);
    Ok(())
}

/// The bounds a numeric `format` implies. The generated models' integers are
/// 64-bit signed, so a 64-bit or pointer-sized integer takes the bound of its
/// sign alone, and a 128-bit integer has no representation.
fn numeric_format(
    result: &mut Map<String, Value>,
    format: &str,
    integral: bool,
    context: &str,
) -> Result<()> {
    let bounds: (Option<f64>, Option<f64>) = match (format, integral) {
        ("int8", true) => (Some(-128.0), Some(127.0)),
        ("int16", true) => (Some(-32_768.0), Some(32_767.0)),
        ("int32", true) => (Some(-2_147_483_648.0), Some(2_147_483_647.0)),
        ("int64" | "int", true) => (None, None),
        ("uint8", true) => (Some(0.0), Some(255.0)),
        ("uint16", true) => (Some(0.0), Some(65_535.0)),
        ("uint32", true) => (Some(0.0), Some(4_294_967_295.0)),
        ("uint64" | "uint", true) => (Some(0.0), None),
        ("int128" | "uint128", true) => {
            return refuse(format!(
                "{context} format {format} exceeds the generated models' 64-bit integers."
            ))
        }
        ("float", false) => (Some(-FLOAT_LIMIT), Some(FLOAT_LIMIT)),
        ("double", false) => (None, None),
        _ => {
            return refuse(format!(
                "{context} uses unsupported imported format {format} for its type."
            ))
        }
    };
    if let Some(minimum) = bounds.0 {
        tighten(result, "minimum", minimum, context)?;
    }
    if let Some(maximum) = bounds.1 {
        tighten(result, "maximum", maximum, context)?;
    }
    Ok(())
}

/// Whether `schema` admits only `null`.
fn is_null(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
        && schema.as_object().is_some_and(|schema| {
            schema
                .keys()
                .all(|key| matches!(key.as_str(), "type" | "description" | "title"))
        })
}

/// The annotations a union's wrapper carries beside `keyword`; `ignored` names
/// keywords the wrapper may repeat from its members.
fn wrapper_annotations(
    source: &Map<String, Value>,
    keyword: &str,
    ignored: &[&str],
    context: &str,
) -> Result<Map<String, Value>> {
    let mut annotations = Map::new();
    for (key, value) in source {
        if key == keyword || ignored.contains(&key.as_str()) {
            continue;
        }
        if !["description", "title", "default"].contains(&key.as_str()) {
            return refuse(format!("{context} may not combine {keyword} with {key}."));
        }
        annotations.insert(key.clone(), value.clone());
    }
    Ok(annotations)
}

/// `anyOf` pairing one schema with `null` is that schema, nullable.
fn any_of(source: &Map<String, Value>, members: &Value, context: &str) -> Result<Value> {
    let annotations = wrapper_annotations(source, "anyOf", &[], context)?;
    let Some(members) = members.as_array() else {
        return refuse(format!("{context}.anyOf must list schemas."));
    };
    let others: Vec<&Value> = members.iter().filter(|member| !is_null(member)).collect();
    if others.len() != 1 || members.len() != 2 {
        return refuse(format!(
            "{context}.anyOf is representable only as one schema paired with null."
        ));
    }
    nullable(others[0], annotations, &format!("{context}.anyOf"))
}

/// `schema`, nullable: a typed schema names `null` among its types, any other
/// is paired with `{"type": "null"}`.
fn nullable(schema: &Value, annotations: Map<String, Value>, context: &str) -> Result<Value> {
    let inner = convert(schema, context)?;
    let merged = |types: Value| {
        let mut merged = inner.as_object().expect("a converted object").clone();
        merged.insert("type".into(), types);
        for (key, value) in &annotations {
            merged.insert(key.clone(), value.clone());
        }
        Value::Object(merged)
    };
    match inner.get("type") {
        // Any JSON value already admits null.
        Some(Value::String(single)) if single == "json" || single == "null" => Ok(inner),
        Some(Value::String(single))
            if inner.get("oneOf").is_none() && inner.get("enum").is_none() =>
        {
            Ok(merged(json!([single, "null"])))
        }
        Some(Value::Array(types)) if types.iter().any(|member| member == "null") => Ok(inner),
        Some(Value::Array(types)) => {
            let mut types = types.clone();
            types.push("null".into());
            Ok(merged(Value::Array(types)))
        }
        _ => {
            let mut wrapper = annotations;
            wrapper.insert("anyOf".into(), json!([inner, {"type": "null"}]));
            Ok(Value::Object(wrapper))
        }
    }
}

/// A string a union member admits, with the member's description.
type DescribedValue = (String, Option<Value>);

/// The strings a `oneOf` member admits, each with its description, when the
/// member admits strings only and lists them.
fn unit_values(member: &Value) -> Option<Vec<DescribedValue>> {
    let member = member.as_object()?;
    if member.get("type").is_some_and(|kind| kind != "string") {
        return None;
    }
    if member.keys().any(|key| {
        !matches!(
            key.as_str(),
            "type" | "const" | "enum" | "description" | "title"
        )
    }) {
        return None;
    }
    let description = member.get("description").cloned();
    if let Some(Value::String(constant)) = member.get("const") {
        return Some(vec![(constant.clone(), description)]);
    }
    let values = member.get("enum")?.as_array()?;
    values
        .iter()
        .map(|value| value.as_str().map(|value| (value.to_owned(), None)))
        .collect()
}

/// The tag, value and description of a member written as a closed object of
/// one required member.
fn data_variant(member: &Value) -> Option<(&str, &Value, Option<&Value>)> {
    let member = member.as_object()?;
    if member.get("type").is_some_and(|kind| kind != "object")
        || member.get("additionalProperties") != Some(&Value::Bool(false))
        || member.keys().any(|key| {
            !matches!(
                key.as_str(),
                "type"
                    | "properties"
                    | "required"
                    | "additionalProperties"
                    | "description"
                    | "title"
            )
        })
    {
        return None;
    }
    let properties = member.get("properties")?.as_object()?;
    let required = member.get("required")?.as_array()?;
    let [(tag, value)] = properties.iter().collect::<Vec<_>>()[..] else {
        return None;
    };
    (required.len() == 1 && required[0] == *tag).then_some((
        tag.as_str(),
        value,
        member.get("description"),
    ))
}

/// A `oneOf`: a string enum when every member admits listed strings, an
/// internally tagged union when every member is an object carrying one string
/// constant under one required name, or an externally tagged union when every
/// member is a closed object of one required member or admits listed strings.
fn one_of(source: &Map<String, Value>, variants: &Value, context: &str) -> Result<Value> {
    let Some(variants) = variants.as_array().filter(|variants| !variants.is_empty()) else {
        return refuse(format!("{context}.oneOf requires variants."));
    };
    let annotations = wrapper_annotations(source, "oneOf", &["type"], context)?;
    let others: Vec<&Value> = variants.iter().filter(|member| !is_null(member)).collect();
    if others.len() != variants.len() {
        if others.len() != 1 {
            return refuse(format!(
                "{context}.oneOf pairs null with a union; name the union and pair its reference with null."
            ));
        }
        return nullable(others[0], annotations, &format!("{context}.oneOf"));
    }

    // Every member admits listed strings: a string enum.
    let units: Option<Vec<Vec<DescribedValue>>> = variants.iter().map(unit_values).collect();
    if let Some(units) = units {
        let mut values: Vec<Value> = Vec::new();
        let mut descriptions = Map::new();
        for (value, description) in units.into_iter().flatten() {
            if values.iter().any(|listed| listed == value.as_str()) {
                return refuse(format!("{context}.oneOf repeats the value {value:?}."));
            }
            if let Some(description) = description {
                descriptions.insert(value.clone(), description);
            }
            values.push(Value::from(value));
        }
        let mut result = annotations;
        result.insert("type".into(), "string".into());
        result.insert("enum".into(), Value::Array(values));
        if !descriptions.is_empty() {
            result.insert("enumDescriptions".into(), Value::Object(descriptions));
        }
        return Ok(Value::Object(result));
    }

    if let Some(discriminator) = discriminator(variants) {
        return internally_tagged(annotations, variants, &discriminator, context);
    }

    // Every member a closed object of one member or listed strings: an
    // externally tagged union.
    let mut data = Map::new();
    let mut unit_tags: Vec<Value> = Vec::new();
    let mut descriptions = Map::new();
    for (index, member) in variants.iter().enumerate() {
        if let Some((tag, value, description)) = data_variant(member) {
            let mut value = convert(value, &format!("{context}.oneOf.{index}.properties.{tag}"))?;
            if let (Some(description), Some(value)) = (description, value.as_object_mut()) {
                value
                    .entry("description")
                    .or_insert_with(|| description.clone());
            }
            if data.insert(tag.to_owned(), value).is_some() {
                return refuse(format!("{context}.oneOf repeats the tag {tag:?}."));
            }
        } else if let Some(units) = unit_values(member) {
            for (unit, description) in units {
                if let Some(description) = description {
                    descriptions.insert(unit.clone(), description);
                }
                unit_tags.push(Value::from(unit));
            }
        } else {
            return refuse(format!(
                "{context}.oneOf.{index} is neither an object of one tagged member nor a tag; the subset holds a union only as tagged variants."
            ));
        }
    }
    if data.is_empty() {
        return refuse(format!("{context}.oneOf declares no data variant."));
    }
    let mut result = annotations;
    result.remove("default");
    result.insert("oneOf".into(), Value::Object(data));
    if !unit_tags.is_empty() {
        result.insert("enum".into(), Value::Array(unit_tags));
    }
    if !descriptions.is_empty() {
        result.insert("enumDescriptions".into(), Value::Object(descriptions));
    }
    Ok(Value::Object(result))
}

/// The one property every variant requires and declares as a string constant.
fn discriminator(variants: &[Value]) -> Option<String> {
    let first = variants[0].get("properties")?.as_object()?;
    let candidates: Vec<&String> = first
        .keys()
        .filter(|key| {
            variants.iter().all(|variant| {
                variant
                    .get("properties")
                    .and_then(|properties| properties.get(key.as_str()))
                    .and_then(|schema| schema.get("const"))
                    .is_some_and(Value::is_string)
                    && variant
                        .get("required")
                        .and_then(Value::as_array)
                        .is_some_and(|required| required.iter().any(|field| field == key.as_str()))
            })
        })
        .collect();
    match candidates[..] {
        [single] => Some(single.clone()),
        _ => None,
    }
}

/// An internally tagged union: each variant without its tag, under the tag.
fn internally_tagged(
    annotations: Map<String, Value>,
    variants: &[Value],
    discriminator: &str,
    context: &str,
) -> Result<Value> {
    let mut converted = Map::new();
    for (index, variant) in variants.iter().enumerate() {
        let variant_context = format!("{context}.oneOf.{index}");
        let Some(mut object) = variant.as_object().cloned() else {
            return refuse(format!("{variant_context} must be an object schema."));
        };
        if object.get("type").is_some_and(|kind| kind != "object") {
            return refuse(format!("{variant_context} must be an object schema."));
        }
        let tag_schema = object
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .and_then(|properties| properties.remove(discriminator))
            .expect("the discriminator is a property of every variant");
        let annotation_only = tag_schema.as_object().is_some_and(|schema| {
            schema
                .keys()
                .all(|key| matches!(key.as_str(), "const" | "type" | "description" | "title"))
        });
        if !annotation_only || tag_schema.get("type").is_some_and(|kind| kind != "string") {
            return refuse(format!(
                "{variant_context} discriminator must be a string constant."
            ));
        }
        let tag = tag_schema["const"].as_str().expect("a string constant");
        if let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|key| key.as_str() != Some(discriminator));
            if required.is_empty() {
                object.remove("required");
            }
        }
        object.insert("type".into(), "object".into());
        let variant = typed(object, &variant_context)?;
        if converted.insert(tag.into(), variant).is_some() {
            return refuse(format!("{context} repeats discriminator value {tag}."));
        }
    }
    let mut result = annotations;
    result.remove("default");
    result.insert("type".into(), "object".into());
    result.insert("discriminator".into(), discriminator.into());
    result.insert("oneOf".into(), Value::Object(converted));
    Ok(Value::Object(result))
}

//! Compose explicitly named JSON Schema bundles into the generator's schema subset.
use crate::error::{refuse, Error, Result};
use crate::json::Json;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

pub(crate) fn compose(mut document: Json, paths: &[PathBuf]) -> Result<Json> {
    if paths.is_empty() {
        return Ok(document);
    }
    let Some(definitions) = document
        .as_object_mut()
        .and_then(|document| document.get_mut("$defs"))
        .and_then(Json::as_object_mut)
    else {
        return refuse("A projection definition importing schemas must declare $defs.");
    };
    for path in paths {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        let bundle: Value = serde_json::from_str(&text).map_err(|error| Error::Parse {
            path: path.clone(),
            message: error.to_string(),
        })?;
        let Some(imports) = bundle.get("$defs").and_then(Value::as_object) else {
            return refuse(format!(
                "Schema import {} must declare a $defs object.",
                path.display()
            ));
        };
        for (name, schema) in imports {
            if definitions.contains_key(name) {
                return refuse(format!(
                    "Schema import {} conflicts with existing definition {name}.",
                    path.display()
                ));
            }
            let converted = convert(schema, &format!("{}.$defs.{name}", path.display()))?;
            definitions.insert(
                name,
                Json::parse(&converted.to_string())
                    .map_err(|error| Error::Definition(error.to_string()))?,
            );
        }
    }
    Ok(document)
}

fn convert(schema: &Value, context: &str) -> Result<Value> {
    if schema == &Value::Bool(true) {
        return Ok(
            json!({"type": "json", "description": "An opaque JSON value declared by the imported contract."}),
        );
    }
    let Some(source) = schema.as_object() else {
        return refuse(format!("{context} requires a representable object schema."));
    };
    if source.contains_key("$ref") {
        if source
            .keys()
            .any(|key| !["$ref", "default", "description", "title"].contains(&key.as_str()))
        {
            return refuse(format!(
                "{context} cannot combine $ref with validation keywords."
            ));
        }
        return Ok(schema.clone());
    }
    if let Some(variants) = source.get("oneOf") {
        return tagged_union(source, variants, context);
    }
    let allowed = [
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "const",
        "minLength",
        "minimum",
        "maximum",
        "format",
        "default",
        "description",
        "title",
    ];
    for key in source.keys() {
        if !allowed.contains(&key.as_str()) {
            return refuse(format!(
                "{context} uses unsupported imported schema keyword {key}."
            ));
        }
    }
    let mut result = source.clone();
    if let Some(format) = result.remove("format") {
        // These numeric format names annotate the Rust representation. Their range is explicit.
        let limits: (i64, i64) = match format.as_str() {
            Some("uint8") => (0, 255),
            Some("uint16") => (0, 65_535),
            Some("uint32") => (0, 4_294_967_295),
            Some("int8") => (-128, 127),
            Some("int16") => (-32_768, 32_767),
            Some("int32") => (-2_147_483_648, 2_147_483_647),
            _ => {
                return refuse(format!(
                    "{context} uses unsupported imported format {format}."
                ))
            }
        };
        if result.get("type").and_then(Value::as_str) != Some("integer") {
            return refuse(format!("{context} numeric format requires type integer."));
        }
        for (name, limit, tighter) in [("minimum", limits.0, true), ("maximum", limits.1, false)] {
            let limit = json!(limit);
            if let Some(existing) = result.get(name) {
                let Some(bound) = existing.as_f64() else {
                    return refuse(format!("{context}.{name} must be a number."));
                };
                let limit_value = limit.as_f64().unwrap();
                if (tighter && bound >= limit_value) || (!tighter && bound <= limit_value) {
                    continue;
                }
            }
            result.insert(name.into(), limit);
        }
    }
    if let Some(properties) = source.get("properties") {
        let Some(properties) = properties.as_object() else {
            return refuse(format!("{context}.properties must be an object."));
        };
        let mut fields = Map::new();
        for (name, schema) in properties {
            fields.insert(
                name.clone(),
                convert(schema, &format!("{context}.properties.{name}"))?,
            );
        }
        result.insert("properties".into(), Value::Object(fields));
    }
    if let Some(items) = source.get("items") {
        result.insert("items".into(), convert(items, &format!("{context}.items"))?);
    }
    if let Some(additional) = source.get("additionalProperties") {
        if additional != &Value::Bool(false) {
            result.insert(
                "additionalProperties".into(),
                convert(additional, &format!("{context}.additionalProperties"))?,
            );
        }
    }
    Ok(Value::Object(result))
}

fn tagged_union(source: &Map<String, Value>, variants: &Value, context: &str) -> Result<Value> {
    for key in source.keys() {
        if !["oneOf", "title", "description"].contains(&key.as_str()) {
            return refuse(format!(
                "{context} tagged unions may not combine oneOf with {key}."
            ));
        }
    }
    let Some(variants) = variants.as_array().filter(|variants| !variants.is_empty()) else {
        return refuse(format!("{context}.oneOf requires object variants."));
    };
    let Some(properties) = variants[0].get("properties").and_then(Value::as_object) else {
        return refuse(format!("{context} union variants require properties."));
    };
    let candidates = properties
        .keys()
        .filter(|key| {
            variants.iter().all(|variant| {
                variant
                    .get("properties")
                    .and_then(|properties| properties.get(*key))
                    .and_then(|schema| schema.get("const"))
                    .and_then(Value::as_str)
                    .is_some()
                    && variant
                        .get("required")
                        .and_then(Value::as_array)
                        .is_some_and(|fields| {
                            fields
                                .iter()
                                .any(|field| field.as_str() == Some(key.as_str()))
                        })
            })
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return refuse(format!(
            "{context} requires one common required string discriminator."
        ));
    }
    let discriminator = candidates[0];
    let mut converted = Map::new();
    for variant in variants {
        let Some(mut object) = variant.as_object().cloned() else {
            return refuse(format!("{context} union variants must be objects."));
        };
        let properties = object
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .unwrap();
        let tag_schema = properties.remove(discriminator).unwrap();
        if !tag_schema
            .as_object()
            .unwrap()
            .keys()
            .all(|key| matches!(key.as_str(), "const" | "type"))
            || tag_schema.get("type").is_some_and(|kind| kind != "string")
        {
            return refuse(format!(
                "{context} discriminator must be a string constant."
            ));
        }
        let tag = tag_schema["const"].as_str().unwrap();
        object
            .get_mut("required")
            .and_then(Value::as_array_mut)
            .unwrap()
            .retain(|key| key.as_str() != Some(discriminator));
        if converted
            .insert(tag.into(), convert(&Value::Object(object), context)?)
            .is_some()
        {
            return refuse(format!("{context} repeats discriminator value {tag}."));
        }
    }
    let mut result = json!({"type": "object", "discriminator": discriminator, "oneOf": converted});
    for key in ["description", "title"] {
        if let Some(value) = source.get(key) {
            result[key] = value.clone();
        }
    }
    Ok(result)
}

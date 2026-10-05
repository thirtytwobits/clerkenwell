//! The declarations a definition's collaboration section implies.
//!
//! Every collaborative entity has an authoring state: the projection a client
//! keeps a replica of the entity's document in step with. Its parameters name
//! the document, and its snapshot and patch are the same for every entity. The
//! generator writes them into the definition before checking it, so the
//! definition declares none of them.

use crate::definition::Fields;
use crate::error::{refuse, Result};
use crate::json::{Json, Object};

/// The snapshot every authoring-state projection delivers.
pub const AUTHORING_STATE: &str = "AuthoringState";
/// The patch every authoring-state projection delivers.
pub const AUTHORING_STATE_PATCH: &str = "AuthoringStatePatch";
/// The block of peers every authoring state allocates its subscriber.
pub const AUTHORING_PEER_BLOCK: &str = "AuthoringPeerBlock";

const AUTHORING_STATE_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["schema_version", "accepted_frontier_base64", "update_base64", "etag", "exchange_modes", "peer_block"],
  "properties": {
    "schema_version": {
      "type": "integer",
      "description": "The layout version the document's operations are written in."
    },
    "accepted_frontier_base64": {
      "type": "string",
      "description": "The frontier of the accepted state this delivery leaves its client holding."
    },
    "update_base64": {
      "type": "string",
      "description": "The accepted operations this delivery adds: every one in a snapshot, and in a patch those after the frontier its client held."
    },
    "etag": {
      "type": "string",
      "description": "The accepted state's etag."
    },
    "exchange_modes": {
      "type": "array",
      "items": { "type": "string", "enum": ["incremental", "bootstrap"] },
      "description": "How the store accepts an import of this document."
    },
    "peer_block": { "$ref": "#/$defs/AuthoringPeerBlock" }
  }
}"##;

const AUTHORING_PEER_BLOCK_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["nonce", "base", "index_bits"],
  "description": "A block of peers allocated to the subscriber: every peer whose bits above the low index_bits are base's. An import names the block by its nonce.",
  "properties": {
    "nonce": { "type": "string" },
    "base": {
      "type": "string",
      "description": "The block's first peer, in decimal."
    },
    "index_bits": { "type": "integer" }
  }
}"##;

const AUTHORING_STATE_PATCH_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": false,
  "required": ["kind"],
  "properties": {
    "kind": { "type": "string", "enum": ["replace", "remove"] },
    "state": { "$ref": "#/$defs/AuthoringState" }
  }
}"##;

/// `document` with every collaborative entity's authoring-state projection
/// and parameters, and the snapshot and patch they share, declared.
pub(crate) fn expand(mut document: Object) -> Result<Object> {
    let collaborative: Vec<(String, Object)> = document
        .get("collaboration")
        .and_then(|collaboration| collaboration.get("entities"))
        .and_then(Json::as_object)
        .map(|entities| {
            entities
                .iter()
                .filter_map(|(name, entity)| {
                    let authoring = entity.get("authoringState")?.as_object()?.clone();
                    Some((name.to_owned(), authoring))
                })
                .collect()
        })
        .unwrap_or_default();
    if collaborative.is_empty() {
        return Ok(document);
    }

    let mut defs = section(&document, "$defs");
    for reserved in [AUTHORING_STATE, AUTHORING_STATE_PATCH, AUTHORING_PEER_BLOCK] {
        if defs.contains_key(reserved) {
            return refuse(format!(
                "$defs.{reserved} is the authoring state every collaborative entity shares; the generator declares it."
            ));
        }
    }
    defs.insert(AUTHORING_STATE, parse(AUTHORING_STATE_SCHEMA));
    defs.insert(AUTHORING_STATE_PATCH, parse(AUTHORING_STATE_PATCH_SCHEMA));
    defs.insert(AUTHORING_PEER_BLOCK, parse(AUTHORING_PEER_BLOCK_SCHEMA));

    let entities = section(&document, "entities");
    let mut projections = section(&document, "projections");
    let mut expanded: Vec<(&str, String)> = Vec::new();
    for (name, authoring) in &collaborative {
        // The semantic checks refuse a collaboration entry for an unknown entity.
        let Some(entity) = entities.get(name).and_then(Json::as_object) else {
            continue;
        };
        let context = format!("collaboration.entities.{name}.authoringState");
        let projection = authoring.str_field("projection");
        if let Some((_, other)) = expanded
            .iter()
            .find(|(declared, _)| *declared == projection)
        {
            return refuse(format!(
                "{context}.projection names {projection}, which is {other}'s authoring state."
            ));
        }
        if projections.contains_key(projection) {
            return refuse(format!(
                "{context}.projection names {projection}, which projections declares; the generator declares every authoring state."
            ));
        }
        expanded.push((projection, name.clone()));
        let params_name = format!("{name}AuthoringParams");
        if defs.contains_key(&params_name) {
            return refuse(format!(
                "$defs.{params_name} holds {name}'s authoring-state parameters; the generator declares it."
            ));
        }
        let mut params: Vec<&str> = authoring.opt_strs_field("storeParams").unwrap_or_default();
        if authoring.opt_str("document").is_none() {
            let id = entity.str_field("id");
            if params.contains(&id) {
                return refuse(format!(
                    "{context}.storeParams names {id}, the parameter that names {name}'s document."
                ));
            }
            params.insert(0, id);
        }
        defs.insert(params_name.as_str(), parameters(&params));
        projections.insert(
            projection,
            parse(&format!(
                r##"{{
                  "description": {description},
                  "params": {{ "$ref": {params} }},
                  "snapshot": {{ "$ref": "#/$defs/{AUTHORING_STATE}" }},
                  "patch": {{ "$ref": "#/$defs/{AUTHORING_STATE_PATCH}" }},
                  "dependsOn": [{entity}],
                  "materialization": {{
                    "strategy": "replaceOrRemove",
                    "snapshotMode": "field",
                    "snapshotField": "state",
                    "removeMode": "nullSnapshot"
                  }}
                }}"##,
                description = quote(&format!(
                    "The accepted state of one {name} document, for keeping a replica of it."
                )),
                params = quote(&format!("#/$defs/{params_name}")),
                entity = quote(name),
            )),
        );
    }
    document.insert("$defs", defs.into());
    document.insert("projections", projections.into());
    Ok(document)
}

/// An object schema requiring every one of `names` as a string.
fn parameters(names: &[&str]) -> Json {
    let properties: Vec<String> = names
        .iter()
        .map(|name| format!(r#"{}: {{ "type": "string" }}"#, quote(name)))
        .collect();
    let required: Vec<String> = names.iter().map(|name| quote(name)).collect();
    parse(&format!(
        r#"{{ "type": "object", "additionalProperties": false, "required": [{}], "properties": {{ {} }} }}"#,
        required.join(", "),
        properties.join(", "),
    ))
}

fn section(document: &Object, key: &str) -> Object {
    document
        .get(key)
        .and_then(Json::as_object)
        .cloned()
        .unwrap_or_default()
}

fn parse(text: &str) -> Json {
    Json::parse(text).expect("the generator's own declarations are JSON")
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).expect("a string serialises")
}

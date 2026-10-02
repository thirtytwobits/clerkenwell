//! The session protocol's wire form, as `wire-protocol.json` records it.

use schemars::generate::{SchemaGenerator, SchemaSettings};
use schemars::JsonSchema;
use serde_json::{json, Map, Value};

use crate::authoring::{AuthoringHeld, AuthoringState};
use crate::transport::{
    ProjectionErrorEnvelope, ProjectionMutationAccepted, ProjectionMutationCommand,
    ProjectionResyncAccepted, ProjectionResyncCommand, ProjectionSubscribeAccepted,
    ProjectionSubscribeCommand, ProjectionTransportEvent, ProjectionUnsubscribeAccepted,
    ProjectionUnsubscribeCommand, PROJECTION_MUTATE_METHOD, PROJECTION_RESYNC_METHOD,
    PROJECTION_SUBSCRIBE_METHOD, PROJECTION_UNSUBSCRIBE_METHOD, PROJECTION_UPDATE_NOTIFICATION,
};

/// The file beside the crate's manifest that holds the record.
pub const WIRE_PROTOCOL_RECORD: &str = "wire-protocol.json";

/// The JSON Schema of every message the protocol exchanges: each method's
/// parameters and result, each notification's parameters, the refusal
/// envelope and the authoring state. Snapshots, patches and mutation results
/// are the application's, so any value stands for them. The schema carries
/// no annotations, so documentation can change without changing the record.
pub fn wire_protocol() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let mut methods = Map::new();
    let mut method = |name: &str, params: Value, result: Value| {
        methods.insert(
            name.to_owned(),
            json!({ "params": params, "result": result }),
        );
    };
    method(
        PROJECTION_SUBSCRIBE_METHOD,
        schema::<ProjectionSubscribeCommand>(&mut generator),
        schema::<ProjectionSubscribeAccepted>(&mut generator),
    );
    method(
        PROJECTION_RESYNC_METHOD,
        schema::<ProjectionResyncCommand>(&mut generator),
        schema::<ProjectionResyncAccepted>(&mut generator),
    );
    method(
        PROJECTION_UNSUBSCRIBE_METHOD,
        schema::<ProjectionUnsubscribeCommand>(&mut generator),
        schema::<ProjectionUnsubscribeAccepted>(&mut generator),
    );
    method(
        PROJECTION_MUTATE_METHOD,
        schema::<ProjectionMutationCommand>(&mut generator),
        schema::<ProjectionMutationAccepted<Value>>(&mut generator),
    );
    let mut notifications = Map::new();
    notifications.insert(
        PROJECTION_UPDATE_NOTIFICATION.to_owned(),
        json!({ "params": schema::<ProjectionTransportEvent<Value, Value>>(&mut generator) }),
    );
    let refusal = schema::<ProjectionErrorEnvelope>(&mut generator);
    let authoring = json!({
        "state": schema::<AuthoringState>(&mut generator),
        "held": schema::<AuthoringHeld>(&mut generator),
    });
    let mut definitions = generator.take_definitions(true);
    definitions.values_mut().for_each(strip_annotations);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "methods": methods,
        "notifications": notifications,
        "refusal": refusal,
        "authoring": authoring,
        "$defs": definitions,
    })
}

fn schema<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
    let mut schema = generator.subschema_for::<T>().to_value();
    strip_annotations(&mut schema);
    schema
}

/// The keywords that annotate a schema without constraining a value.
const ANNOTATIONS: &[&str] = &["description", "title", "examples"];

/// The keywords whose value is a map from names to schemas.
const SCHEMA_MAPS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "dependentSchemas",
];

/// The keywords whose value is a value, not a schema.
const VALUES: &[&str] = &["const", "default", "enum"];

/// Removes the annotations from `schema` and every schema within it.
pub fn strip_annotations(schema: &mut Value) {
    let Value::Object(keywords) = schema else {
        return;
    };
    keywords.retain(|keyword, _| !ANNOTATIONS.contains(&keyword.as_str()));
    for (keyword, value) in keywords.iter_mut() {
        if VALUES.contains(&keyword.as_str()) {
            continue;
        }
        match value {
            Value::Object(map) if SCHEMA_MAPS.contains(&keyword.as_str()) => {
                map.values_mut().for_each(strip_annotations);
            }
            Value::Object(_) => strip_annotations(value),
            Value::Array(schemas) => schemas.iter_mut().for_each(strip_annotations),
            _ => {}
        }
    }
}

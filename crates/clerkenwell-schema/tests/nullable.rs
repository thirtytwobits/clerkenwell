//! An optional nullable member: absent, `null` and a value are three values,
//! and each is written back as it was read.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Reply {
    #[serde(default)]
    #[serde(deserialize_with = "clerkenwell_schema::present")]
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<Option<String>>,
}

fn read(value: Value) -> Reply {
    serde_json::from_value(value).expect("a reply")
}

#[test]
fn absence_null_and_a_value_are_distinct_and_round_trip() {
    let note = "a note";
    let cases = [
        (json!({}), None),
        (json!({"note": null}), Some(None)),
        (json!({"note": note}), Some(Some(note.to_owned()))),
    ];
    for (written, expected) in cases {
        let reply = read(written.clone());
        assert_eq!(reply.note, expected);
        assert_eq!(serde_json::to_value(&reply).unwrap(), written);
    }
}

#[test]
fn a_value_of_another_type_is_refused() {
    assert!(serde_json::from_value::<Reply>(json!({"note": 1})).is_err());
}

#[test]
fn the_schema_leaves_the_member_optional_and_admits_null() {
    let schema = serde_json::to_value(schemars::schema_for!(Reply)).unwrap();
    let required = schema.get("required").and_then(Value::as_array);
    assert!(required.is_none_or(|required| !required.contains(&json!("note"))));
    let validator = jsonschema::validator_for(&schema).expect("a schema");
    for admitted in [json!({}), json!({"note": null}), json!({"note": "text"})] {
        assert!(validator.is_valid(&admitted), "{admitted}");
    }
    assert!(!validator.is_valid(&json!({"note": 1})));
}

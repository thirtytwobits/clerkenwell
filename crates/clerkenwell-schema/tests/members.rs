//! An open object's other members: flattened beside its declared members, they
//! read and write every member once, and refuse a member the object declares.

use std::collections::BTreeMap;

use clerkenwell_schema::{DeclaredMembers, OtherMembers, SchemaConstraint, SchemaConstraintValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
struct Card {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(flatten)]
    other_members: OtherMembers<Card, String>,
}

impl DeclaredMembers for Card {
    const DECLARED_MEMBERS: &'static [&'static str] = &["kind", "title"];
}

#[test]
fn an_open_object_round_trips_every_member() {
    let wire = json!({ "kind": "note", "title": "Plan", "colour": "red", "size": "large" });
    let card: Card = serde_json::from_value(wire.clone()).expect("an open object parses");
    assert_eq!(card.kind, "note");
    assert_eq!(card.title.as_deref(), Some("Plan"));
    assert_eq!(
        card.other_members.get("colour").map(String::as_str),
        Some("red")
    );
    assert_eq!(
        card.other_members.get("size").map(String::as_str),
        Some("large")
    );
    assert_eq!(card.other_members.len(), 2);
    assert_eq!(serde_json::to_value(&card).expect("serialises"), wire);
}

#[test]
fn other_members_hold_no_declared_member() {
    let card: Card =
        serde_json::from_value(json!({ "kind": "note", "colour": "red" })).expect("parses");
    assert!(card.other_members.keys().all(|name| name == "colour"));
}

#[test]
fn inserting_a_declared_member_is_refused() {
    let mut members = OtherMembers::<Card, String>::new();
    let refused = members
        .insert("kind", "task".to_owned())
        .expect_err("kind is declared");
    assert_eq!(refused.name(), "kind");
    assert!(members.is_empty());

    assert_eq!(members.insert("colour", "red".to_owned()), Ok(None));
    assert_eq!(
        members.insert("colour", "blue".to_owned()),
        Ok(Some("red".to_owned()))
    );
}

#[test]
fn a_map_holding_a_declared_member_is_refused() {
    let map = BTreeMap::from([
        ("colour".to_owned(), "red".to_owned()),
        ("title".to_owned(), "Plan".to_owned()),
    ]);
    let refused = OtherMembers::<Card, String>::try_from(map).expect_err("title is declared");
    assert_eq!(refused.name(), "title");

    let map = BTreeMap::from([("colour".to_owned(), "red".to_owned())]);
    let members = OtherMembers::<Card, String>::try_from(map.clone()).expect("accepted");
    assert_eq!(members.into_map(), map);
}

#[test]
fn other_members_read_alone_refuse_a_declared_member() {
    let read: Result<OtherMembers<Card, String>, _> =
        serde_json::from_value(json!({ "colour": "red", "kind": "note" }));
    assert!(read.is_err());
}

#[test]
fn other_members_serialise_in_key_order() {
    let wire = r#"{"kind":"note","zeta":"z","alpha":"a","mu":"m"}"#;
    let card: Card = serde_json::from_str(wire).expect("parses");
    assert_eq!(
        serde_json::to_string(&card).expect("serialises"),
        r#"{"kind":"note","alpha":"a","mu":"m","zeta":"z"}"#
    );
}

#[test]
fn the_schema_declares_the_members_and_types_the_others() {
    let schema = serde_json::to_value(schemars::schema_for!(Card)).expect("a schema");
    assert_eq!(schema["properties"]["kind"]["type"], "string");
    assert!(schema["properties"].get("title").is_some());
    assert!(schema["properties"].get("other_members").is_none());
    assert_eq!(
        schema["additionalProperties"]["type"],
        Value::from("string")
    );
    assert_eq!(schema["required"], json!(["kind"]));
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
struct Grid {
    name: String,
    #[serde(flatten)]
    other_members: OtherMembers<Grid, Vec<String>>,
}

impl DeclaredMembers for Grid {
    const DECLARED_MEMBERS: &'static [&'static str] = &["name"];
    const OTHER_MEMBER_CONSTRAINTS: &'static [SchemaConstraint] = &[
        SchemaConstraint {
            path: &["items"],
            keyword: "minLength",
            value: SchemaConstraintValue::Integer(2),
        },
        SchemaConstraint {
            path: &["items"],
            keyword: "const",
            value: SchemaConstraintValue::String("xy"),
        },
    ];
}

#[test]
fn the_schema_carries_the_other_members_constraints() {
    let schema = serde_json::to_value(schemars::schema_for!(Grid)).expect("a schema");
    let items = &schema["additionalProperties"]["items"];
    assert_eq!(items["type"], "string");
    assert_eq!(items["minLength"], 2);
    assert_eq!(items["const"], "xy");
    assert!(schema["properties"]["name"].get("minLength").is_none());
}

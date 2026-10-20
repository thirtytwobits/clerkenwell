//! A record's members read and write as a map, in key order, and the schema
//! derived for a record carries its values' constraints.

use std::collections::BTreeMap;

use clerkenwell_schema::{
    RecordConstraints, RecordMembers, SchemaConstraint, SchemaConstraintValue,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

enum LabelConstraints {}

impl RecordConstraints for LabelConstraints {
    const VALUE_CONSTRAINTS: &'static [SchemaConstraint] = &[
        SchemaConstraint {
            path: "",
            keyword: "minLength",
            value: SchemaConstraintValue::Integer(2),
        },
        SchemaConstraint {
            path: "",
            keyword: "default",
            value: SchemaConstraintValue::String("xy"),
        },
    ];
}

type Labels = RecordMembers<LabelConstraints, String>;

enum GridConstraints {}

impl RecordConstraints for GridConstraints {
    const VALUE_CONSTRAINTS: &'static [SchemaConstraint] = &[
        SchemaConstraint {
            path: "/items/additionalProperties",
            keyword: "maximum",
            value: SchemaConstraintValue::Number(2.5),
        },
        SchemaConstraint {
            path: "/items",
            keyword: "default",
            value: SchemaConstraintValue::Json(r#"{"a": 1}"#),
        },
    ];
}

type Grid = RecordMembers<GridConstraints, Vec<BTreeMap<String, f64>>>;

#[test]
fn a_record_round_trips_in_key_order() {
    let labels: Labels =
        serde_json::from_str(r#"{"zeta":"z","alpha":"a","mu":"m"}"#).expect("parses");
    assert_eq!(
        serde_json::to_string(&labels).expect("serialises"),
        r#"{"alpha":"a","mu":"m","zeta":"z"}"#
    );
}

#[test]
fn a_record_reads_and_writes_as_its_map() {
    let mut labels = Labels::new();
    labels.insert("colour".to_owned(), "red".to_owned());
    labels
        .entry("size".to_owned())
        .or_insert_with(|| "large".to_owned());
    assert_eq!(labels.get("colour").map(String::as_str), Some("red"));

    let map = BTreeMap::from([
        ("colour".to_owned(), "red".to_owned()),
        ("size".to_owned(), "large".to_owned()),
    ]);
    assert_eq!(labels, Labels::from(map.clone()));
    assert_eq!(labels.clone().into_map(), map);
    assert_eq!(labels, map.into_iter().collect::<Labels>());
}

/// A record iterates mutably and extends as its map does, where a `for` loop
/// or a generic bound names the trait rather than dereferencing.
#[test]
fn a_record_iterates_mutably_and_extends_as_its_map() {
    fn extend_with_size(members: &mut impl Extend<(String, String)>) {
        members.extend([("size".to_owned(), "large".to_owned())]);
    }

    let mut labels = Labels::from(BTreeMap::from([("colour".to_owned(), "red".to_owned())]));
    extend_with_size(&mut labels);
    for (_, value) in &mut labels {
        value.make_ascii_uppercase();
    }
    assert_eq!(
        labels.into_map(),
        BTreeMap::from([
            ("colour".to_owned(), "RED".to_owned()),
            ("size".to_owned(), "LARGE".to_owned()),
        ])
    );
}

#[test]
fn the_schema_carries_the_values_constraints() {
    let schema = serde_json::to_value(schemars::schema_for!(Labels)).expect("a schema");
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"]["type"], "string");
    assert_eq!(schema["additionalProperties"]["minLength"], 2);
    assert_eq!(schema["additionalProperties"]["default"], "xy");
}

#[test]
fn the_schema_carries_constraints_within_items_and_nested_maps() {
    let schema = serde_json::to_value(schemars::schema_for!(Grid)).expect("a schema");
    let items = &schema["additionalProperties"]["items"];
    assert_eq!(items["default"], json!({ "a": 1 }));
    assert_eq!(items["additionalProperties"]["type"], "number");
    assert_eq!(items["additionalProperties"]["maximum"], 2.5);
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
struct Holder {
    labels: Labels,
    #[schemars(transform = SchemaConstraint::new("", "default", SchemaConstraintValue::Integer(-1)))]
    count: i64,
    #[schemars(transform = SchemaConstraint::new("/additionalProperties", "const", SchemaConstraintValue::Boolean(true)))]
    flags: Option<BTreeMap<String, bool>>,
}

/// A record keeps its constraints as a field, and a field's own constraint
/// transforms reach into its type's schema.
#[test]
fn a_field_carries_its_record_s_and_its_own_constraints() {
    let schema = serde_json::to_value(schemars::schema_for!(Holder)).expect("a schema");
    let properties = &schema["properties"];
    assert_eq!(properties["labels"]["additionalProperties"]["minLength"], 2);
    assert_eq!(properties["flags"]["additionalProperties"]["const"], true);
    assert_eq!(properties["count"]["default"], -1);
}

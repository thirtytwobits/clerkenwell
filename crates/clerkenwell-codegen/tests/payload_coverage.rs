//! Every `json` field is either counted as undescribed or says why arbitrary
//! JSON is its type, and a projection carries an undescribed payload when its
//! snapshot reaches one.

mod common;

use clerkenwell_codegen::json::Json;
use clerkenwell_codegen::{collect_payload_coverage, PayloadCoverage};
use common::{notebook_dir, refusal, validate, with_defs};

fn coverage_of(document: Json) -> PayloadCoverage {
    let definition = validate(document).expect("the probe definition is valid");
    collect_payload_coverage(&definition).expect("coverage collects")
}

/// A projection whose snapshot `$def` can be replaced without disturbing any
/// other rule: its patch carries the snapshot whole in a field.
const SUBSTITUTABLE_PROJECTION: &str = "tasks.board";
const SUBSTITUTABLE_SNAPSHOT: &str = "TaskBoardSnapshot";

#[test]
fn an_undescribed_json_field_is_counted_and_says_where_it_is() {
    let coverage = coverage_of(with_defs(
        r#"{ "ProbeSnapshot": {
            "type": "object", "additionalProperties": false, "required": ["blob"],
            "properties": { "blob": { "type": "json" } } } }"#,
    ));
    assert!(coverage
        .undescribed
        .contains(&"ProbeSnapshot.blob".to_owned()));
}

#[test]
fn a_reasoned_json_field_is_counted_apart_from_the_undescribed_ones() {
    let reason = "Carries a JSON Schema document.";
    let coverage = coverage_of(with_defs(&format!(
        r#"{{ "ProbeSnapshot": {{
            "type": "object", "additionalProperties": false, "required": ["blob"],
            "properties": {{ "blob": {{ "type": "json", "opaqueReason": "{reason}" }} }} }} }}"#
    )));
    assert!(!coverage
        .undescribed
        .contains(&"ProbeSnapshot.blob".to_owned()));
    assert!(coverage
        .reasoned
        .contains(&("ProbeSnapshot.blob".to_owned(), reason.to_owned())));
}

#[test]
fn nested_json_fields_are_counted_wherever_they_sit() {
    let coverage = coverage_of(with_defs(
        r#"{ "ProbeSnapshot": {
            "type": "object", "additionalProperties": false,
            "properties": {
                "rows": { "type": "array", "items": { "type": "json" } },
                "index": { "type": "object", "additionalProperties": { "type": "json" } } } } }"#,
    ));
    assert!(coverage
        .undescribed
        .contains(&"ProbeSnapshot.rows[]".to_owned()));
    assert!(coverage
        .undescribed
        .contains(&"ProbeSnapshot.index{}".to_owned()));
}

/// Both ends are substituted so the case proves reachability from its own data
/// rather than from whatever the example happens to hold.
#[test]
fn a_projection_counts_as_carrying_one_when_its_snapshot_reaches_it_through_a_ref() {
    let coverage = coverage_of(with_defs(&format!(
        r##"{{
            "{SUBSTITUTABLE_SNAPSHOT}": {{
                "type": "object", "additionalProperties": false, "required": ["document"],
                "properties": {{ "document": {{ "$ref": "#/$defs/ProbeDocument" }} }} }},
            "ProbeDocument": {{
                "type": "object", "additionalProperties": false, "required": ["widgets"],
                "properties": {{ "widgets": {{ "type": "json" }} }} }} }}"##
    )));
    assert!(coverage
        .undescribed
        .contains(&"ProbeDocument.widgets".to_owned()));
    assert!(coverage
        .projections_carrying_undescribed
        .contains(&SUBSTITUTABLE_PROJECTION.to_owned()));
}

#[test]
fn a_described_payload_keeps_its_projection_out_of_the_count() {
    let coverage = coverage_of(with_defs(&format!(
        r#"{{ "{SUBSTITUTABLE_SNAPSHOT}": {{
            "type": "object", "additionalProperties": false, "required": ["title"],
            "properties": {{ "title": {{ "type": "string" }} }} }} }}"#
    )));
    assert!(!coverage
        .undescribed
        .iter()
        .any(|entry| entry.starts_with(SUBSTITUTABLE_SNAPSHOT)));
    assert!(!coverage
        .projections_carrying_undescribed
        .contains(&SUBSTITUTABLE_PROJECTION.to_owned()));
}

#[test]
fn opaque_reason_is_rejected_where_the_field_is_not_json() {
    let message = refusal(with_defs(
        r#"{ "ProbeSnapshot": {
            "type": "object", "additionalProperties": false, "required": ["title"],
            "properties": { "title": { "type": "string", "opaqueReason": "Carries a JSON Schema document." } } } }"#,
    ));
    assert!(
        message.contains("opaqueReason is only meaningful on a json schema"),
        "{message}"
    );
}

#[test]
fn opaque_reason_is_rejected_when_it_says_nothing() {
    let message = refusal(with_defs(
        r#"{ "ProbeSnapshot": {
            "type": "object", "additionalProperties": false, "required": ["blob"],
            "properties": { "blob": { "type": "json", "opaqueReason": "   " } } } }"#,
    ));
    assert!(
        message.contains("opaqueReason must be a non-empty string"),
        "{message}"
    );
}

/// A tagged union's Rust and TypeScript renderings differ in kind rather than
/// spelling, so each way of declaring a malformed one must fail rather than
/// render something plausible.
#[test]
fn a_malformed_tagged_union_is_rejected() {
    let empty_variant = r#"{ "type": "object", "additionalProperties": false, "properties": {} }"#;
    let cases = [
        (
            "no discriminator",
            format!(r#"{{ "type": "object", "oneOf": {{ "a": {empty_variant} }} }}"#),
            "discriminator must name the property carrying the tag",
        ),
        (
            "no variants at all",
            r#"{ "type": "object", "discriminator": "kind", "oneOf": {} }"#.to_owned(),
            "at least one variant",
        ),
        (
            "a variant restating the tag",
            format!(
                r#"{{ "type": "object", "discriminator": "kind", "oneOf": {{
                    "a": {{ "type": "object", "additionalProperties": false, "properties": {{ "kind": {{ "type": "string" }} }} }},
                    "b": {empty_variant} }} }}"#
            ),
            "which the tag already carries",
        ),
        (
            "properties beside the variants",
            format!(
                r#"{{ "type": "object", "discriminator": "kind", "properties": {{}},
                    "oneOf": {{ "a": {empty_variant}, "b": {empty_variant} }} }}"#
            ),
            "may not declare properties beside its oneOf",
        ),
    ];
    for (name, schema, expected) in cases {
        let message = refusal(with_defs(&format!(r#"{{ "ProbeUnion": {schema} }}"#)));
        assert!(message.contains(expected), "{name}: {message}");
    }
}

/// A union of one is still a union: internal tagging means callers send the tag.
#[test]
fn a_single_variant_tagged_union_is_accepted() {
    validate(with_defs(
        r#"{ "ProbeUnion": {
            "type": "object", "discriminator": "kind",
            "oneOf": { "only": { "type": "object", "additionalProperties": false,
                "properties": { "title": { "type": "string" } } } } } }"#,
    ))
    .expect("a single-variant union is valid");
}

#[test]
fn a_tagged_unions_variants_are_walked_for_undescribed_payloads() {
    let coverage = coverage_of(with_defs(
        r#"{ "ProbeUnion": {
            "type": "object", "discriminator": "kind",
            "oneOf": {
                "plain": { "type": "object", "additionalProperties": false, "properties": { "title": { "type": "string" } } },
                "opaque": { "type": "object", "additionalProperties": false, "properties": { "blob": { "type": "json" } } } } } }"#,
    ));
    assert!(coverage
        .undescribed
        .contains(&"ProbeUnion|opaque.blob".to_owned()));
    assert!(!coverage
        .undescribed
        .iter()
        .any(|entry| entry.starts_with("ProbeUnion|plain")));
}

/// The variant's payload belongs to the union's `$def`, so a projection
/// reaching the union carries it.
#[test]
fn a_projection_reaching_a_tagged_union_carries_its_undescribed_payload() {
    let coverage = coverage_of(with_defs(&format!(
        r##"{{
            "{SUBSTITUTABLE_SNAPSHOT}": {{
                "type": "object", "additionalProperties": false, "required": ["state"],
                "properties": {{ "state": {{ "$ref": "#/$defs/ProbeUnion" }} }} }},
            "ProbeUnion": {{
                "type": "object", "discriminator": "kind",
                "oneOf": {{ "opaque": {{ "type": "object", "additionalProperties": false,
                    "properties": {{ "blob": {{ "type": "json" }} }} }} }} }} }}"##
    )));
    assert!(coverage
        .projections_carrying_undescribed
        .contains(&SUBSTITUTABLE_PROJECTION.to_owned()));
}

/// The guard that ratchets these counts reads the published report; a report
/// without the section would pass it silently.
#[test]
fn the_published_report_carries_the_section_the_guard_ratchets() {
    let text = std::fs::read_to_string(
        notebook_dir().join("generated/notebook.projections.coverage.json"),
    )
    .expect("the example's coverage report is committed");
    let report = Json::parse(&text).expect("the report is JSON");
    let figure = |name: &str| {
        report
            .pointer(&format!("/payloads/{name}"))
            .and_then(Json::as_f64)
            .unwrap_or_else(|| panic!("payloads.{name} must be published as a number"))
    };
    let listed = |name: &str| {
        report
            .pointer(&format!("/payloads/{name}"))
            .and_then(Json::as_array)
            .unwrap_or_else(|| panic!("payloads.{name} must be published as a list"))
            .len() as f64
    };
    assert_eq!(figure("undescribedCount"), listed("undescribed"));
    assert_eq!(figure("reasonedCount"), listed("reasoned"));
    assert!(figure("projectionsCarryingUndescribed") <= figure("projectionCount"));
}

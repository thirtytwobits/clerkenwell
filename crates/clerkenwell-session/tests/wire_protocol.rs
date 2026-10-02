//! The session protocol's wire form matches its record.

use std::path::Path;

use clerkenwell_session::testing::{strip_annotations, wire_protocol, WIRE_PROTOCOL_RECORD};
use serde_json::json;

#[test]
fn the_wire_protocol_matches_its_record() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(WIRE_PROTOCOL_RECORD);
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&wire_protocol()).expect("the schema serialises")
    );
    if std::env::var_os("CLERKENWELL_WRITE_RECORDS").is_some() {
        std::fs::write(&path, &text).expect("the record is writable");
    }
    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        recorded == text,
        "{} differs from the wire protocol. Run \
         `CLERKENWELL_WRITE_RECORDS=1 cargo test -p clerkenwell-session --test wire_protocol` \
         to rewrite it.",
        path.display()
    );
}

#[test]
fn a_record_carries_no_annotations_and_keeps_every_constraint() {
    let mut schema = json!({
        "title": "Note",
        "description": "A note.",
        "type": "object",
        "properties": {
            "description": { "description": "Its text.", "type": "string", "default": { "description": "kept" } },
            "kind": { "const": { "title": "kept" }, "examples": ["gone"] },
            "tags": { "type": "array", "items": { "title": "A tag.", "enum": [{ "description": "kept" }] } }
        },
        "oneOf": [{ "description": "A variant.", "required": ["kind"] }],
        "$defs": { "Title": { "title": "Gone", "type": "string" } }
    });
    strip_annotations(&mut schema);
    assert_eq!(
        schema,
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "default": { "description": "kept" } },
                "kind": { "const": { "title": "kept" } },
                "tags": { "type": "array", "items": { "enum": [{ "description": "kept" }] } }
            },
            "oneOf": [{ "required": ["kind"] }],
            "$defs": { "Title": { "type": "string" } }
        })
    );
}

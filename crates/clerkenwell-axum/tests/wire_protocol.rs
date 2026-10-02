//! The JSON-RPC framing of the session protocol matches its record.

use std::path::Path;

use clerkenwell_axum::testing::{wire_protocol, WIRE_PROTOCOL_RECORD};

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
         `CLERKENWELL_WRITE_RECORDS=1 cargo test -p clerkenwell-axum --test wire_protocol` \
         to rewrite it.",
        path.display()
    );
}

//! A field its parent requires is present whenever the parent is; any other
//! optional field is present only when written.

mod support;

use clerkenwell_schema::GeneratedCollaborationEntitySpec;
use serde_json::{json, Value};
use support::{reread, seed, wire_document, BOARD, NOTE};

fn without(document: &Value, parent: &str, member: &str) -> Value {
    let mut document = document.clone();
    document[parent]
        .as_object_mut()
        .unwrap_or_else(|| panic!("{parent} is an object"))
        .remove(member);
    document
}

fn round_trip(plan: &'static GeneratedCollaborationEntitySpec, document: &Value) -> Value {
    reread(plan, &seed(plan, document))
}

/// A board whose one archived column holds `column`.
fn archived(column: Value) -> Value {
    let mut board = wire_document(BOARD);
    board["archive"] = json!([column]);
    board
}

#[test]
fn an_optional_member_of_a_present_group_left_out_reads_back_absent() {
    let note = without(&wire_document(NOTE), "meta", "reviewer");

    let read = round_trip(NOTE, &note);

    assert!(read["meta"].is_object(), "the group is present: {read}");
    assert!(read["meta"].get("reviewer").is_none(), "{read}");
}

#[test]
fn a_member_a_present_group_requires_reads_back_present_when_written_empty() {
    let mut note = wire_document(NOTE);
    note["source"] = json!({ "url": "", "excerpt": "Quoted." });

    let read = round_trip(NOTE, &note);

    assert_eq!(read["source"]["url"], json!(""), "{read}");
    assert_eq!(read["source"]["excerpt"], note["source"]["excerpt"]);
}

#[test]
fn an_optional_group_left_out_reads_back_absent() {
    let mut note = wire_document(NOTE);
    note.as_object_mut().expect("a note").remove("source");

    let read = round_trip(NOTE, &note);

    assert!(read.get("source").is_none(), "{read}");
}

#[test]
fn a_member_an_item_requires_reads_back_present_when_written_empty() {
    let board = archived(json!({
        "column_id": "archived-1",
        "title": "",
        "cards": [{
            "card_id": "card-1",
            "text": "",
            "done": false,
            "note": { "$mime": "text/plain", "value": "Kept." }
        }]
    }));

    let read = round_trip(BOARD, &board);

    let column = &read["archive"][0];
    assert_eq!(column["title"], json!(""), "{read}");
    assert_eq!(column["cards"][0]["text"], json!(""), "{read}");
}

#[test]
fn a_sequence_an_item_requires_reads_back_empty_when_left_out() {
    let board = archived(json!({ "column_id": "archived-1", "title": "Old" }));

    let read = round_trip(BOARD, &board);

    assert_eq!(read["archive"][0]["cards"], json!([]), "{read}");
}

#[test]
fn an_optional_member_of_an_item_left_out_reads_back_absent() {
    let board = archived(json!({ "column_id": "archived-1", "title": "Old", "cards": [] }));

    let read = round_trip(BOARD, &board);

    assert!(read["archive"][0].get("wip_limit").is_none(), "{read}");
}

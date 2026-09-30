//! A replica writes each edit against the document it holds, operations it
//! adopted from other writers included.

mod support;

use clerkenwell_doc::CollaborationReplica;
use serde_json::{json, Value};
use support::{hydrate, read, reread, seed, wire_document, NOTE};

fn edit(replica: &mut CollaborationReplica, change: impl FnOnce(&mut Value)) {
    let mut note = read(replica);
    change(&mut note);
    replica.replace_document(&note).expect("edit");
}

#[test]
fn restoring_a_value_another_writer_replaced_is_written() {
    let original = wire_document(NOTE);
    let base = seed(NOTE, &original)
        .export_update_base64()
        .expect("export");
    let mut ours = hydrate(NOTE, &base);
    let mut renamer = hydrate(NOTE, &base);
    edit(&mut renamer, |note| note["title"] = json!("renamed"));

    ours.adopt_versioned_update_base64(
        NOTE.schema_version,
        &renamer.export_update_base64().expect("export"),
    )
    .expect("adopt the rename");
    edit(&mut ours, |note| note["title"] = original["title"].clone());

    assert_eq!(read(&ours)["title"], original["title"]);
    assert_eq!(reread(NOTE, &ours)["title"], original["title"]);
}

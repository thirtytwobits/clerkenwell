//! Concurrent edits from independent replicas converge on one document in
//! either import order, and each edit survives the merge.

mod support;

use serde_json::{json, Value};
use support::*;

fn column_ids(document: &Value) -> Vec<String> {
    identities(document, "/columns", "column_id")
}

fn card_ids(document: &Value, column: usize) -> Vec<String> {
    identities(document, &format!("/columns/{column}/cards"), "card_id")
}

fn assert_unique(identities: &[String]) {
    let mut unique = identities.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        identities.len(),
        "{identities:?} repeats an identity"
    );
}

#[test]
fn concurrent_edits_to_different_keyed_items_keep_both() {
    let base = board();
    let one = with(&base, "/columns/0/title", json!("Next up"));
    let two = with(&base, "/columns/1/cards/0/labels", json!(["shipped"]));

    let merged = converge(BOARD, &base, &one, &two);

    assert_eq!(merged["columns"][0]["title"], "Next up");
    assert_eq!(
        merged["columns"][1]["cards"][0]["labels"],
        json!(["shipped"])
    );
}

#[test]
fn concurrent_structured_ordered_and_optional_edits_converge() {
    let base = wire_document(NOTE);
    let mut one = base.clone();
    one["tags"] = json!(["tags-value", "one"]);
    items_mut(&mut one, "/attachments").push(json!({ "file_name": "one.pdf", "size_bytes": 1 }));
    one["properties"]["one"] = json!("first");
    remove(&mut one, "/estimate");
    let mut two = base.clone();
    two["tags"] = json!(["two", "tags-value"]);
    items_mut(&mut two, "/attachments").push(json!({ "file_name": "two.pdf", "size_bytes": 2 }));
    two["properties"]["two"] = json!("second");
    two["extras"]["two"] = json!({ "nested": [1, 2] });
    remove(&mut two, "/archived_at");

    let merged = converge(NOTE, &base, &one, &two);

    assert_eq!(merged["tags"], json!(["two", "tags-value", "one"]));
    for file_name in ["one.pdf", "two.pdf"] {
        assert!(
            items(&merged, "/attachments")
                .iter()
                .any(|attachment| attachment["file_name"] == file_name),
            "{file_name}"
        );
    }
    assert_eq!(merged["properties"]["one"], "first");
    assert_eq!(merged["properties"]["two"], "second");
    assert_eq!(merged["extras"]["two"], json!({ "nested": [1, 2] }));
    assert!(merged.get("estimate").is_none(), "{merged}");
    assert!(merged.get("archived_at").is_none(), "{merged}");
}

#[test]
fn independently_seeded_keyed_items_with_one_identity_converge_once() {
    let mut left = board();
    left["columns"][0]["title"] = json!("Left title");
    let mut right = board();
    right["columns"][0]["cards"][0]["text"] = json!("Right text");
    let replica = seed(BOARD, &left);
    replica
        .import_versioned_update_base64(
            BOARD.schema_version,
            &seed(BOARD, &right)
                .export_update_base64()
                .expect("export right"),
        )
        .expect("import right");

    let merged = read(&replica);
    assert_eq!(column_ids(&merged), column_ids(&board()));
    for column in 0..column_ids(&merged).len() {
        assert_eq!(card_ids(&merged, column), card_ids(&board(), column));
    }
}

#[test]
fn concurrent_nested_records_map_keys_and_prose_converge() {
    let base = board();
    let mut one = base.clone();
    items_mut(&mut one, "/columns").push(column("one", "From one", vec![card("one-card", "One")]));
    items_mut(&mut one, "/columns/0/cards").push(card("one-extra", "One extra"));
    one["columns"][1]["cards"][0]["text"] = json!("Plan the release carefully");
    let mut two = base.clone();
    items_mut(&mut two, "/columns").push(column("two", "From two", vec![]));
    items_mut(&mut two, "/columns/0/cards").push(card("two-extra", "Two extra"));
    two["columns"][1]["cards"][0]["text"] = json!("Now plan the release");

    let merged = converge(BOARD, &base, &one, &two);

    let columns = column_ids(&merged);
    for id in ["todo", "done", "one", "two"] {
        assert!(columns.contains(&id.to_string()), "{id} in {columns:?}");
    }
    assert_unique(&columns);
    let todo = card_ids(&merged, 0);
    for id in ["draft", "review", "one-extra", "two-extra"] {
        assert!(todo.contains(&id.to_string()), "{id} in {todo:?}");
    }
    assert_unique(&todo);
    assert_eq!(
        merged["columns"][1]["cards"][0]["text"],
        "Now plan the release carefully"
    );
}

#[test]
fn nested_keyed_sequences_converge_in_both_import_orders() {
    let base = board();
    let mut one = base.clone();
    items_mut(&mut one, "/columns/0/cards").reverse();
    items_mut(&mut one, "/columns/1/cards").push(card("retro", "Retrospective"));
    let mut two = base.clone();
    items_mut(&mut two, "/columns/0/cards").push(card("triage", "Triage"));
    items_mut(&mut two, "/columns/1/cards").remove(0);

    let merged = converge(BOARD, &base, &one, &two);

    assert_eq!(card_ids(&merged, 0), ["review", "draft", "triage"]);
    assert_eq!(card_ids(&merged, 1), ["retro"]);
}

#[test]
fn concurrent_keyed_additions_both_survive_once() {
    let base = board();
    let mut one = base.clone();
    items_mut(&mut one, "/columns").push(column("blocked", "Blocked", vec![]));
    let mut two = base.clone();
    items_mut(&mut two, "/columns").push(column("icebox", "Icebox", vec![]));

    let merged = converge(BOARD, &base, &one, &two);

    let columns = column_ids(&merged);
    for id in ["todo", "done", "blocked", "icebox"] {
        assert!(columns.contains(&id.to_string()), "{id} in {columns:?}");
    }
    assert_unique(&columns);
}

#[test]
fn an_item_removed_on_one_replica_stays_removed_after_an_unrelated_edit_merges() {
    let base = board();
    let mut removed = base.clone();
    items_mut(&mut removed, "/columns").remove(1);
    let edited = with(&base, "/columns/0/title", json!("Edited"));

    let merged = converge(BOARD, &base, &removed, &edited);

    assert_eq!(column_ids(&merged), ["todo"]);
    assert_eq!(merged["columns"][0]["title"], "Edited");
}

#[test]
fn concurrent_reorders_converge_on_one_order_of_the_same_items() {
    let mut base = board();
    items_mut(&mut base, "/columns").push(column("later", "Later", vec![]));
    let mut one = base.clone();
    items_mut(&mut one, "/columns").rotate_right(1);
    let mut two = base.clone();
    items_mut(&mut two, "/columns").rotate_left(1);

    let merged = converge(BOARD, &base, &one, &two);

    let mut columns = column_ids(&merged);
    columns.sort();
    let mut expected = column_ids(&base);
    expected.sort();
    assert_eq!(columns, expected);
}

#[test]
fn concurrent_edits_to_different_parts_of_a_structured_document_both_survive() {
    let base = wire_document(NOTE);
    let one = with(&base, "/layout/columns", json!(3));
    let two = with(&base, "/layout/grid", json!([[1, 2], [3]]));

    let merged = converge(NOTE, &base, &one, &two);

    assert_eq!(merged["layout"]["columns"], 3);
    assert_eq!(merged["layout"]["grid"], json!([[1, 2], [3]]));
}

#[test]
fn a_structured_document_array_without_item_identity_is_replaced_whole() {
    let base = with(&wire_document(NOTE), "/layout/grid", json!([[1], [2]]));
    let one = with(&base, "/layout/grid", json!([[1], [2], [3]]));
    let two = with(&base, "/layout/grid", json!([[1], [2], [4]]));

    let merged = converge(NOTE, &base, &one, &two);

    assert!(
        merged["layout"]["grid"] == one["layout"]["grid"]
            || merged["layout"]["grid"] == two["layout"]["grid"],
        "one replica's array wins whole: {}",
        merged["layout"]["grid"]
    );
}

#[test]
fn concurrent_edits_to_structured_document_prose_merge() {
    let base = with(&wire_document(NOTE), "/layout/caption", json!("Chapter"));
    let one = with(&base, "/layout/caption", json!("Chapter One"));
    let two = with(&base, "/layout/caption", json!("A Chapter"));

    let merged = converge(NOTE, &base, &one, &two);

    assert_eq!(merged["layout"]["caption"], "A Chapter One");
}

fn with_panels(panels: Value) -> Value {
    with(&wire_document(NOTE), "/layout/panels", panels)
}

#[test]
fn concurrent_additions_to_a_structured_document_list_with_item_identity_both_survive() {
    let base = with_panels(json!([{ "id": "one", "label": "One" }]));
    let one = with_panels(json!([
        { "id": "one", "label": "One" },
        { "id": "from-one", "label": "From one" }
    ]));
    let two = with_panels(json!([
        { "id": "one", "label": "One" },
        { "id": "from-two", "label": "From two" }
    ]));

    let merged = converge(NOTE, &base, &one, &two);

    let panels = items(&merged, "/layout/panels");
    let ids = identities(&merged, "/layout/panels", "id");
    assert_eq!(ids.len(), 3, "{ids:?}");
    for id in ["one", "from-one", "from-two"] {
        assert!(ids.contains(&id.to_string()), "{id} in {ids:?}");
    }
    assert!(panels.contains(&json!({ "id": "from-one", "label": "From one" })));
}

#[test]
fn editing_one_entry_of_a_structured_document_list_with_item_identity_leaves_its_siblings() {
    let base = with_panels(json!([
        { "id": "one", "label": "One" },
        { "id": "two", "label": "Two" }
    ]));
    let one = with(&base, "/layout/panels/0/label", json!("Edited by one"));
    let two = with(&base, "/layout/panels/1/label", json!("Edited by two"));

    let merged = converge(NOTE, &base, &one, &two);

    assert_eq!(
        items(&merged, "/layout/panels"),
        &vec![
            json!({ "id": "one", "label": "Edited by one" }),
            json!({ "id": "two", "label": "Edited by two" })
        ]
    );
}

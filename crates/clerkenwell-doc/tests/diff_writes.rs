//! Whole-document writes become the edits between the two documents, so two
//! writers who each rewrote the document from the same base merge instead of
//! colliding.

mod support;

use clerkenwell_notebook::GeneratedCollaborationEntitySpec;
use serde_json::{json, Value};
use support::*;

/// A text field somewhere in a notebook document, and the document it is in.
fn text_fields() -> Vec<(
    &'static GeneratedCollaborationEntitySpec,
    Value,
    &'static str,
)> {
    vec![
        (NOTE, wire_document(NOTE), "/summary"),
        (NOTE, wire_document(NOTE), "/body/value"),
        (BOARD, board(), "/columns/1/cards/0/text"),
        (BOARD, board(), "/columns/0/cards/1/note/value"),
    ]
}

#[test]
fn concurrent_rewrites_of_prose_keep_the_shared_text_once() {
    for (plan, document, pointer) in text_fields() {
        let base = with(&document, pointer, json!("The keeper watches the harbour."));
        let appended = with(
            &base,
            pointer,
            json!("The keeper watches the harbour. At dawn."),
        );
        let prepended = with(
            &base,
            pointer,
            json!("Quietly, the keeper watches the harbour."),
        );

        let merged = converge(plan, &base, &appended, &prepended);

        assert_eq!(
            merged.pointer(pointer),
            Some(&json!("Quietly, the keeper watches the harbour. At dawn.")),
            "{}{pointer}",
            plan.name
        );
    }
}

#[test]
fn concurrent_rewrites_of_an_ordered_list_keep_both_additions() {
    for (plan, document, pointer) in [
        (NOTE, wire_document(NOTE), "/tags"),
        (BOARD, board(), "/columns/0/cards/0/labels"),
    ] {
        let base = with(&document, pointer, json!(["harbour", "keeper"]));
        let appended = with(&base, pointer, json!(["harbour", "keeper", "night"]));
        let prepended = with(&base, pointer, json!(["lamp", "harbour", "keeper"]));

        let merged = converge(plan, &base, &appended, &prepended);

        assert_eq!(
            merged.pointer(pointer),
            Some(&json!(["lamp", "harbour", "keeper", "night"])),
            "{}{pointer}",
            plan.name
        );
    }
}

#[test]
fn a_structured_entry_edited_on_one_side_survives_an_addition_on_the_other() {
    let base = with(
        &wire_document(NOTE),
        "/attachments",
        json!([
            { "file_name": "minutes.pdf", "size_bytes": 2048 },
            { "file_name": "agenda.pdf", "size_bytes": 512 }
        ]),
    );
    let first = base["attachments"][0].clone();
    let edited_entry = json!({ "file_name": "minutes.pdf", "size_bytes": 4096, "url": "https://example.org/minutes.pdf" });
    let added_entry = json!({ "file_name": "actions.pdf", "size_bytes": 128 });
    let edited = with(&base, "/attachments/0", edited_entry.clone());
    let mut added = base.clone();
    items_mut(&mut added, "/attachments").push(added_entry.clone());

    let merged = converge(NOTE, &base, &edited, &added);
    let attachments = items(&merged, "/attachments");

    assert!(attachments.contains(&edited_entry), "{merged}");
    assert!(attachments.contains(&added_entry), "{merged}");
    assert!(
        !attachments.contains(&first),
        "the entry one side rewrote does not come back"
    );
    assert_eq!(attachments.len(), items(&base, "/attachments").len() + 1);
}

#[test]
fn a_reorder_and_an_addition_keep_every_keyed_item_once() {
    for (pointer, identity, item) in [
        ("/columns", "column_id", column("later", "Later", vec![])),
        ("/columns/0/cards", "card_id", card("triage", "Triage")),
    ] {
        let base = board();
        let mut reordered = base.clone();
        items_mut(&mut reordered, pointer).reverse();
        let mut added = base.clone();
        items_mut(&mut added, pointer).push(item.clone());

        let merged = converge(BOARD, &base, &reordered, &added);

        let mut expected = identities(&reordered, pointer, identity);
        expected.push(item[identity].as_str().expect("identity").to_string());
        assert_eq!(
            identities(&merged, pointer, identity),
            expected,
            "{pointer}"
        );
    }
}

#[test]
fn a_rewritten_list_holds_exactly_the_written_entries() {
    let alphabet = ["a", "b", "c", "d", "e", "f"];
    // A fixed linear congruential sequence keeps the cases reproducible.
    let mut state = 0x2545_f491_u64;
    let mut next = |bound: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (state >> 33) as usize % bound
    };
    let mut writer = seed(NOTE, &wire_document(NOTE));
    for _ in 0..200 {
        let length = next(8);
        let tags = (0..length)
            .map(|_| alphabet[next(alphabet.len())])
            .collect::<Vec<_>>();
        let rewrite = with(&read(&writer), "/tags", json!(tags));
        writer.replace_document(&rewrite).expect("rewrite tags");
        assert_eq!(reread(NOTE, &writer)["tags"], json!(tags));
    }
}

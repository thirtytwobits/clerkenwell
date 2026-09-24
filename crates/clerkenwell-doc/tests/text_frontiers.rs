//! Text read and consumed at a captured frontier: consumption removes exactly
//! the captured characters and keeps whatever was typed after the capture.

mod support;

use std::collections::HashMap;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clerkenwell_doc::loro::{ExportMode, LoroDoc};
use clerkenwell_doc::{CollaborationLoroError, LoroAuthoringDocument};
use serde_json::json;
use support::*;

const FIELD: &str = "summary";

fn container() -> &'static str {
    field(NOTE, FIELD).container.expect("summary container")
}

fn no_identities() -> HashMap<String, String> {
    HashMap::new()
}

fn note_with_summary(summary: &str) -> LoroAuthoringDocument {
    seed(
        NOTE,
        &with(&wire_document(NOTE), "/summary", json!(summary)),
    )
}

fn accept(authority: &mut LoroAuthoringDocument, update: &str) {
    authority
        .adopt_versioned_update_base64(NOTE.schema_version, update)
        .expect("accept update");
}

fn typed(client: &LoroDoc, offset: usize, text: &str) -> String {
    client
        .get_text(container())
        .insert_utf8(offset, text)
        .expect("type");
    client.commit();
    export_all(client)
}

#[test]
fn consumption_removes_only_captured_characters_at_every_unicode_boundary() {
    let original = "before 🦊 e\u{301} 👩‍💻\n後";
    let later = "new 🐈";
    for offset in original
        .char_indices()
        .map(|(index, _)| index)
        .chain([original.len()])
    {
        let mut authority = note_with_summary(original);
        let frontier = authority.accepted_frontier_base64();
        let client = raw(&authority);
        accept(&mut authority, &typed(&client, offset, later));
        let before = authority.export_update_base64().expect("export");
        assert_eq!(
            authority
                .text_at_frontier(FIELD, &no_identities(), &frontier)
                .expect("captured text"),
            original
        );
        let prepared = authority
            .prepare_text_replacement_at_frontier(FIELD, &no_identities(), &frontier, original, "")
            .expect("prepare consumption");
        assert_eq!(
            authority.export_update_base64().expect("export"),
            before,
            "preparation must not consume text"
        );
        accept(&mut authority, &prepared);
        accept(&mut authority, &prepared);
        client
            .import(&BASE64.decode(&prepared).expect("base64"))
            .expect("client imports consumption");
        assert_eq!(read(&authority)["summary"], later, "offset {offset}");
        assert_eq!(
            client.get_text(container()).to_string(),
            later,
            "offset {offset}"
        );
    }
}

#[test]
fn captured_replacement_preserves_concurrent_text_and_other_fields() {
    let mut authority = seed(NOTE, &wire_document(NOTE));
    let frontier = authority.accepted_frontier_base64();
    let captured = authority
        .text_at_frontier(FIELD, &no_identities(), &frontier)
        .expect("captured text");
    let client = raw(&authority);
    let later = "Next draft 🦊";
    let replacement = "Replacement 👩‍💻";
    client
        .get_text(container())
        .insert_utf8(captured.len(), later)
        .expect("type after the capture");
    let body = field(NOTE, "body").container.expect("body container");
    client
        .get_text(body)
        .insert_utf8(0, later)
        .expect("type in another field");
    client.commit();
    let prepared = authority
        .prepare_text_replacement_at_frontier(
            FIELD,
            &no_identities(),
            &frontier,
            &captured,
            replacement,
        )
        .expect("prepare replacement");
    accept(&mut authority, &export_all(&client));
    let other = read(&authority)["body"].clone();
    accept(&mut authority, &prepared);
    client
        .import(&BASE64.decode(&prepared).expect("base64"))
        .expect("client imports replacement");

    let actual = read(&authority);
    let text = actual["summary"].as_str().expect("summary");
    assert!(text.contains(replacement), "{text}");
    assert!(text.contains(later), "{text}");
    assert_eq!(text.len(), replacement.len() + later.len());
    assert_eq!(client.get_text(container()).to_string(), text);
    assert_eq!(actual["body"], other);
}

#[test]
fn a_prefix_prepared_at_a_frontier_keeps_the_captured_and_the_concurrent_text() {
    let mut authority = note_with_summary("Captured.");
    let frontier = authority.accepted_frontier_base64();
    let client = raw(&authority);
    accept(
        &mut authority,
        &typed(&client, "Captured.".len(), " Later."),
    );
    let prepared = authority
        .prepare_text_prefix_at_frontier(FIELD, &no_identities(), &frontier, "Captured.", "First. ")
        .expect("prepare prefix");
    accept(&mut authority, &prepared);
    assert_eq!(read(&authority)["summary"], "First. Captured. Later.");
}

#[test]
fn invalid_capture_or_target_cannot_change_the_live_document() {
    let authority = seed(NOTE, &wire_document(NOTE));
    let frontier = authority.accepted_frontier_base64();
    let before = authority.export_update_base64().expect("export");
    assert!(matches!(
        authority.prepare_text_replacement_at_frontier(
            FIELD,
            &no_identities(),
            &frontier,
            "incorrect capture",
            ""
        ),
        Err(CollaborationLoroError::TextCaptureMismatch)
    ));
    for target in ["missing", "title", "tags", "columns.*.cards.*.text"] {
        assert!(
            authority
                .prepare_text_replacement_at_frontier(target, &no_identities(), &frontier, "", "")
                .is_err(),
            "{target}"
        );
    }
    let unrelated = seed(NOTE, &wire_document(NOTE)).accepted_frontier_base64();
    assert!(matches!(
        authority.text_at_frontier(FIELD, &no_identities(), &unrelated),
        Err(CollaborationLoroError::UnknownFrontier)
    ));
    assert!(authority
        .text_at_frontier(FIELD, &no_identities(), "invalid base64")
        .is_err());
    assert_eq!(authority.export_update_base64().expect("export"), before);
}

#[test]
fn keyed_text_targets_keep_their_mime_type_and_refuse_deleted_records() {
    let mut authority = seed(BOARD, &board());
    let mut document = read(&authority);
    let path = "columns.*.cards.*.note";
    let identities = HashMap::from([
        (
            "column_id".to_string(),
            document["columns"][1]["column_id"]
                .as_str()
                .expect("column")
                .to_string(),
        ),
        (
            "card_id".to_string(),
            document["columns"][1]["cards"][0]["card_id"]
                .as_str()
                .expect("card")
                .to_string(),
        ),
    ]);
    let mime = document["columns"][1]["cards"][0]["note"]["$mime"].clone();
    let frontier = authority.accepted_frontier_base64();
    let captured = authority
        .text_at_frontier(path, &identities, &frontier)
        .expect("captured card note");
    assert_eq!(
        json!(captured),
        document["columns"][1]["cards"][0]["note"]["value"]
    );

    let replacement = "changed 🦊";
    let prepared = authority
        .prepare_text_replacement_at_frontier(path, &identities, &frontier, &captured, replacement)
        .expect("prepare");
    authority
        .adopt_versioned_update_base64(BOARD.schema_version, &prepared)
        .expect("accept");
    let actual = read(&authority);
    assert_eq!(
        actual["columns"][1]["cards"][0]["note"]["value"],
        replacement
    );
    assert_eq!(actual["columns"][1]["cards"][0]["note"]["$mime"], mime);
    let mut untouched = actual.clone();
    set(
        &mut untouched,
        "/columns/1/cards/0/note/value",
        document["columns"][1]["cards"][0]["note"]["value"].clone(),
    );
    assert_eq!(
        untouched,
        at_revision(BOARD, &board(), REVISION),
        "only the target changed"
    );

    let other_column = HashMap::from([
        (
            "column_id".to_string(),
            document["columns"][0]["column_id"]
                .as_str()
                .expect("column")
                .to_string(),
        ),
        ("card_id".to_string(), identities["card_id"].clone()),
    ]);
    assert!(
        authority
            .text_at_frontier(path, &other_column, &frontier)
            .is_err(),
        "a card is addressed within its own column"
    );

    items_mut(&mut document, "/columns/1/cards").clear();
    authority
        .replace_document(&document)
        .expect("delete the card");
    assert!(
        authority
            .prepare_text_replacement_at_frontier(path, &identities, &frontier, &captured, "")
            .is_err(),
        "a deleted record cannot be resurrected by consumption"
    );
}

#[test]
fn an_empty_capture_cannot_consume_later_typing() {
    let mut authority = note_with_summary("");
    let frontier = authority.accepted_frontier_base64();
    let client = raw(&authority);
    let later = "Keep this 👩‍💻";
    accept(&mut authority, &typed(&client, 0, later));
    let prepared = authority
        .prepare_text_replacement_at_frontier(FIELD, &no_identities(), &frontier, "", "")
        .expect("prepare");
    accept(&mut authority, &prepared);
    assert_eq!(read(&authority)["summary"], later);
}

#[test]
fn frontier_inclusion_requires_causal_observation_not_matching_visible_text() {
    let mut authority = seed(NOTE, &wire_document(NOTE));
    let base = authority.accepted_frontier_base64();
    let first = raw(&authority);
    let second = raw(&authority);
    for replica in [&first, &second] {
        replica
            .get_text(container())
            .insert(0, "same")
            .expect("type");
        replica.commit();
    }
    let frontier = |doc: &LoroDoc| BASE64.encode(doc.state_frontiers().encode());
    let (left, right) = (frontier(&first), frontier(&second));
    assert!(
        authority.frontier_includes(&left, &base).is_err(),
        "unknown operations must reject"
    );
    for replica in [&first, &second] {
        accept(
            &mut authority,
            &BASE64.encode(replica.export(ExportMode::all_updates()).expect("export")),
        );
    }
    let includes = |capture: &str, required: &str| {
        authority
            .frontier_includes(capture, required)
            .expect("known frontiers")
    };
    assert!(includes(&left, &base));
    assert!(!includes(&base, &left));
    assert!(!includes(&left, &right));
    assert!(!includes(&right, &left));
    let joined = authority.accepted_frontier_base64();
    assert!(includes(&joined, &left));
    assert!(includes(&joined, &right));
    assert!(includes(&joined, &joined));
}

//! A replica writes its operations under the peer it is given, and names the
//! peers an update holds operations of.

mod support;

use std::collections::HashMap;

use clerkenwell_doc::CollaborationReplica;
use serde_json::json;
use support::*;

#[test]
fn a_replica_writes_under_the_peer_it_is_given() {
    let created = CollaborationReplica::from_document(NOTE, &wire_document(NOTE), 41)
        .expect("a replica under peer 41");
    let update = created.export_update_base64().expect("export");

    assert_eq!(
        CollaborationReplica::peers_in_update(&update).expect("peers"),
        [41]
    );

    let mut edited = hydrate(NOTE, &update);
    edited.set_peer(42).expect("peer 42");
    let frontier = edited.accepted_frontier_base64();
    edited
        .replace_document(&with(&wire_document(NOTE), "/title", json!("Renamed")))
        .expect("edit");
    let edit = edited
        .export_incremental_update_base64(&frontier)
        .expect("export edit");
    assert_eq!(
        CollaborationReplica::peers_in_update(&edit).expect("peers"),
        [42]
    );
}

#[test]
fn the_peers_with_new_operations_are_those_the_replica_lacks() {
    let created = seed(NOTE, &wire_document(NOTE));
    let seeded = created.export_update_base64().expect("export");
    let mut writer = hydrate(NOTE, &seeded);
    writer.set_peer(7).expect("peer 7");
    writer
        .replace_document(&with(&wire_document(NOTE), "/title", json!("Renamed")))
        .expect("edit");
    let everything = writer.export_update_base64().expect("export");

    assert_eq!(
        created
            .peers_with_new_operations(&everything)
            .expect("judge"),
        [7],
        "the seed's peer brings nothing the creator lacks"
    );
    assert!(writer
        .peers_with_new_operations(&everything)
        .expect("judge")
        .is_empty());
}

#[test]
fn prepared_text_is_written_under_the_peer_it_is_prepared_for() {
    let authority = seed(NOTE, &wire_document(NOTE));
    let frontier = authority.accepted_frontier_base64();
    let summary = authority
        .text_at_frontier("summary", &HashMap::new(), &frontier)
        .expect("captured summary");

    let prepared = authority
        .prepare_text_replacement_at_frontier(
            "summary",
            &HashMap::new(),
            &frontier,
            &summary,
            "",
            99,
        )
        .expect("prepare");

    assert_eq!(
        CollaborationReplica::peers_in_update(&prepared).expect("peers"),
        [99]
    );
}

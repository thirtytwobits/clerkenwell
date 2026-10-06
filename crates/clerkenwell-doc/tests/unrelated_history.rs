//! A replica takes only operations that descend from its own history: an
//! update holding a history built separately, even from the same content, is
//! refused and changes nothing.

mod support;

use clerkenwell_doc::{CollaborationReplica, CollaborationReplicaError};
use loro::ExportMode;
use serde_json::json;
use support::*;

/// A replica seeded with the fixture note, and the update it was seeded with.
fn seeded() -> (CollaborationReplica, String) {
    let replica = seed(NOTE, &wire_document(NOTE));
    let update = replica.export_update_base64().expect("export");
    (replica, update)
}

#[test]
fn an_update_holding_a_separately_built_history_is_refused_and_changes_nothing() {
    let (mut accepted, _) = seeded();
    let before = (accepted.accepted_frontier_base64(), read(&accepted));
    // The same content seeded again: the history a re-seed leaves a client.
    let mut separate = seed(NOTE, &wire_document(NOTE));
    separate
        .replace_document(&with(
            &wire_document(NOTE),
            "/title",
            json!("Edited offline"),
        ))
        .expect("edit");
    let update = separate.export_update_base64().expect("export");

    let refused = accepted.adopt_versioned_update_base64(NOTE.schema_version, &update);

    assert!(
        matches!(refused, Err(CollaborationReplicaError::UnrelatedHistory)),
        "{refused:?}"
    );
    assert_eq!(
        (accepted.accepted_frontier_base64(), read(&accepted)),
        before
    );
}

#[test]
fn an_update_from_a_replica_holding_both_histories_is_refused() {
    let (mut accepted, _) = seeded();
    let base = accepted.accepted_frontier_base64();
    let mut separate = seed(NOTE, &wire_document(NOTE));
    separate
        .replace_document(&with(
            &wire_document(NOTE),
            "/title",
            json!("Edited offline"),
        ))
        .expect("edit");
    // A client that took the accepted history into its own without checking.
    let both = raw(&separate);
    both.import(
        &raw(&accepted)
            .export(ExportMode::all_updates())
            .expect("export"),
    )
    .expect("import");
    let after_base = hydrate(NOTE, &export_all(&both))
        .export_incremental_update_base64(&base)
        .expect("what follows the accepted frontier");

    assert!(matches!(
        accepted.adopt_versioned_update_base64(NOTE.schema_version, &after_base),
        Err(CollaborationReplicaError::UnrelatedHistory)
    ));
}

#[test]
fn updates_descending_from_the_replicas_history_are_taken() {
    let (mut accepted, seeded_update) = seeded();
    let base = accepted.accepted_frontier_base64();
    let mut client = hydrate(NOTE, &seeded_update);
    client
        .replace_document(&with(&wire_document(NOTE), "/title", json!("Edited")))
        .expect("edit");

    accepted
        .adopt_versioned_update_base64(
            NOTE.schema_version,
            &client
                .export_incremental_update_base64(&base)
                .expect("export"),
        )
        .expect("an edit made on the replica's history");
    accepted
        .adopt_versioned_update_base64(
            NOTE.schema_version,
            &client.export_update_base64().expect("export"),
        )
        .expect("the whole history again, seed included");

    assert_eq!(read(&accepted)["title"], "Edited");
}

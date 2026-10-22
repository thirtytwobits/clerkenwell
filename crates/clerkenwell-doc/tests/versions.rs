//! A replica reads the version an update reaches, and exports the operations
//! a peer holding a version lacks.

mod support;

use std::collections::BTreeMap;

use clerkenwell_doc::{CollaborationReplica, CollaborationReplicaError};
use serde_json::json;
use support::*;

#[test]
fn an_update_reaches_a_counter_for_each_peer_it_holds_operations_of() {
    let created = seed(NOTE, &wire_document(NOTE));
    let update = created.export_update_base64().expect("export");

    let version = CollaborationReplica::version_of_update(&update).expect("version");

    assert_eq!(
        version.keys().copied().collect::<Vec<_>>(),
        CollaborationReplica::peers_in_update(&update).expect("peers")
    );
    assert!(version.values().all(|counter| *counter > 0));
}

#[test]
fn an_edit_advances_the_counter_of_the_peer_that_wrote_it() {
    let created = seed(NOTE, &wire_document(NOTE));
    let before =
        CollaborationReplica::version_of_update(&created.export_update_base64().expect("export"))
            .expect("version");
    let mut edited = hydrate(NOTE, &created.export_update_base64().expect("export"));
    let writer = peer();
    edited.set_peer(writer).expect("peer");
    edited
        .replace_document(&with(&wire_document(NOTE), "/title", json!("Renamed")))
        .expect("edit");

    let after =
        CollaborationReplica::version_of_update(&edited.export_update_base64().expect("export"))
            .expect("version");

    assert!(after.get(&writer).is_some_and(|counter| *counter > 0));
    assert!(before
        .iter()
        .all(|(peer, counter)| after.get(peer) == Some(counter)));
}

#[test]
fn an_export_since_a_version_brings_a_replica_at_that_version_up_to_date() {
    let created = seed(NOTE, &wire_document(NOTE));
    let held = created.export_update_base64().expect("export");
    let version = CollaborationReplica::version_of_update(&held).expect("version");
    let mut behind = hydrate(NOTE, &held);
    let mut ahead = hydrate(NOTE, &held);
    ahead.set_peer(peer()).expect("peer");
    ahead
        .replace_document(&with(&wire_document(NOTE), "/title", json!("Renamed")))
        .expect("edit");

    let lacking = ahead
        .export_update_since_version_base64(&version)
        .expect("export since");

    assert!(CollaborationReplica::version_of_update(&lacking)
        .expect("version")
        .keys()
        .all(|peer| !version.contains_key(peer)));
    behind
        .adopt_versioned_update_base64(NOTE.schema_version, &lacking)
        .expect("adopt");
    assert_eq!(read(&behind), read(&ahead));
}

#[test]
fn an_export_since_a_version_naming_unseen_peers_holds_what_the_replica_has_beyond_it() {
    let created = seed(NOTE, &wire_document(NOTE));
    let held = created.export_update_base64().expect("export");
    let mut version = CollaborationReplica::version_of_update(&held).expect("version");
    let behind = hydrate(NOTE, &held);
    let mut ahead = hydrate(NOTE, &held);
    ahead.set_peer(peer()).expect("peer");
    ahead
        .replace_document(&with(&wire_document(NOTE), "/title", json!("Renamed")))
        .expect("edit");
    let unseen = u64::MAX >> 1;
    version.insert(unseen, 40);

    let lacking = ahead
        .export_update_since_version_base64(&version)
        .expect("export since");

    let mut behind = behind;
    behind
        .adopt_versioned_update_base64(NOTE.schema_version, &lacking)
        .expect("adopt");
    assert_eq!(read(&behind), read(&ahead));
}

#[test]
fn an_export_since_a_counter_no_operation_reaches_is_refused() {
    let created = seed(NOTE, &wire_document(NOTE));
    let version = BTreeMap::from([(peer(), u64::MAX)]);

    let refused = created
        .export_update_since_version_base64(&version)
        .expect_err("a counter beyond any operation");

    assert!(matches!(
        refused,
        CollaborationReplicaError::InvalidVersion { .. }
    ));
}

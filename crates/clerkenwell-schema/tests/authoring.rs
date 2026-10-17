//! The authoring state's wire form: what it writes it reads back, and it
//! refuses a member it does not declare.

use clerkenwell_schema::{
    AuthoringPeerBlock, AuthoringState, AuthoringStatePatch, AuthoringStatePatchKind,
    CollaborationExchangeMode,
};
use serde_json::{json, Value};

fn state() -> AuthoringState {
    AuthoringState {
        schema_version: 3,
        accepted_frontier_base64: "frontier".to_owned(),
        update_base64: "update".to_owned(),
        etag: "etag".to_owned(),
        exchange_modes: vec![
            CollaborationExchangeMode::Incremental,
            CollaborationExchangeMode::Bootstrap,
        ],
        peer_block: AuthoringPeerBlock {
            nonce: "nonce".to_owned(),
            base: u64::MAX.to_string(),
            index_bits: u32::MAX,
        },
        replaces_held: true,
    }
}

fn read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(value)
}

#[test]
fn an_authoring_state_reads_back_as_it_was_written() {
    let written = state();
    let wire = serde_json::to_value(&written).expect("serialises");
    assert_eq!(read::<AuthoringState>(wire).expect("parses"), written);
}

#[test]
fn every_patch_reads_back_as_it_was_written() {
    for written in [
        AuthoringStatePatch::replace(state()),
        AuthoringStatePatch::remove(),
    ] {
        let wire = serde_json::to_value(&written).expect("serialises");
        assert_eq!(read::<AuthoringStatePatch>(wire).expect("parses"), written);
    }
}

#[test]
fn a_replacement_carries_its_state_and_a_removal_none() {
    let replaced = AuthoringStatePatch::replace(state());
    assert_eq!(replaced.kind, AuthoringStatePatchKind::Replace);
    assert_eq!(replaced.state, Some(state()));

    let removed = AuthoringStatePatch::remove();
    assert_eq!(removed.kind, AuthoringStatePatchKind::Remove);
    let wire = serde_json::to_value(&removed).expect("serialises");
    assert!(wire.get("state").is_none(), "{wire}");
}

#[test]
fn a_member_no_type_declares_is_refused() {
    let mut wire = serde_json::to_value(state()).expect("serialises");
    wire["undeclared"] = json!(true);
    assert!(read::<AuthoringState>(wire).is_err());

    let mut wire = serde_json::to_value(state()).expect("serialises");
    wire["peer_block"]["undeclared"] = json!(true);
    assert!(read::<AuthoringState>(wire).is_err());

    let mut wire = serde_json::to_value(AuthoringStatePatch::remove()).expect("serialises");
    wire["undeclared"] = json!(true);
    assert!(read::<AuthoringStatePatch>(wire).is_err());
}

#[test]
fn an_exchange_mode_the_type_does_not_name_is_refused() {
    let mut wire = serde_json::to_value(state()).expect("serialises");
    wire["exchange_modes"] = json!(["optimisticDocument"]);
    assert!(read::<AuthoringState>(wire).is_err());
}

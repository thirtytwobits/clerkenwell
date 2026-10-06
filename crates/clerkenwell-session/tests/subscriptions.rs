use std::time::Duration;

use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_session::transport::ProjectionTransportEvent;
use clerkenwell_session::{FollowingDelivery, ProjectionSubscriptions};
use serde_json::{json, Value};

type Patch = Value;
type Snapshot = Value;
type Subscriptions = ProjectionSubscriptions<Patch, String>;
type Event = ProjectionTransportEvent<Snapshot, Patch>;

fn subscriptions_of_a_client() -> Subscriptions {
    Subscriptions::new(Principal::new("client", ActorKind::Human))
}

fn held(event: &Event) -> Option<&Value> {
    match event {
        Event::Snapshot { held, .. } | Event::Patch { held, .. } => held.as_ref(),
    }
}

#[test]
fn a_subscriber_that_holds_nothing_is_sent_a_snapshot_naming_what_it_leaves_it_holding() {
    let mut subscriptions = subscriptions_of_a_client();
    let state = "accepted-state".to_string();

    let (accepted, events) = subscriptions.accept_subscription(
        "notes.authoringState".to_string(),
        json!({}),
        None,
        json!({ "note": "snapshot" }),
        Some(state.clone()),
    );

    assert!(!accepted.up_to_date);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        Event::Snapshot { subscription_id, revision, .. }
            if *subscription_id == accepted.subscription_id && *revision == accepted.revision
    ));
    assert_eq!(held(&events[0]), Some(&json!(state)));
    assert_eq!(
        subscriptions
            .get(accepted.subscription_id)
            .and_then(|subscription| subscription.delivered.clone()),
        Some(state)
    );
}

#[test]
fn a_subscriber_that_already_holds_what_it_would_be_sent_is_sent_nothing() {
    let mut subscriptions = subscriptions_of_a_client();
    let state = "accepted-state".to_string();

    let (accepted, events) = subscriptions.accept_subscription(
        "notes.authoringState".to_string(),
        json!({}),
        Some(state.clone()),
        json!({ "note": "unsent" }),
        Some(state.clone()),
    );

    assert!(accepted.up_to_date);
    assert!(events.is_empty());
    assert_eq!(
        subscriptions
            .get(accepted.subscription_id)
            .and_then(|subscription| subscription.delivered.clone()),
        Some(state)
    );
    assert_eq!(subscriptions.diagnostics().resume_up_to_date_count, 1);
}

#[test]
fn a_subscriber_that_holds_an_earlier_state_is_sent_a_snapshot() {
    let mut subscriptions = subscriptions_of_a_client();
    let current = "current-state".to_string();

    let (accepted, events) = subscriptions.accept_subscription(
        "notes.authoringState".to_string(),
        json!({}),
        Some("earlier-state".to_string()),
        json!({ "note": "what the subscriber lacks" }),
        Some(current.clone()),
    );

    assert!(!accepted.up_to_date);
    assert_eq!(events.len(), 1);
    assert_eq!(held(&events[0]), Some(&json!(current)));
    assert_eq!(subscriptions.diagnostics().resume_snapshot_count, 1);
}

#[test]
fn an_update_the_host_cannot_name_leaves_its_client_holding_nothing_named() {
    let mut subscriptions = subscriptions_of_a_client();
    let (accepted, events) = subscriptions.accept_subscription(
        "notes.list".to_string(),
        json!({}),
        None,
        json!([]),
        Some("named".to_string()),
    );
    assert!(held(&events[0]).is_some());

    let patched: Event = subscriptions
        .deliver_patch(
            accepted.subscription_id,
            0,
            json!({ "kind": "upsert" }),
            None,
        )
        .expect("the subscription exists");

    assert!(held(&patched).is_none());
    assert_eq!(
        subscriptions
            .get(accepted.subscription_id)
            .and_then(|subscription| subscription.delivered.clone()),
        None
    );
}

#[test]
fn a_patch_takes_its_client_from_the_revision_it_holds_and_names_what_it_leaves() {
    let mut subscriptions = subscriptions_of_a_client();
    let id = subscriptions.insert("notes.byId".to_string(), json!({}), 3);
    let after = "after-patch".to_string();

    let patched: Event = subscriptions
        .deliver_patch(id, 0, json!({ "kind": "replace" }), Some(after.clone()))
        .expect("the subscription exists");

    assert!(matches!(
        patched,
        Event::Patch { from_revision: 3, to_revision, .. } if to_revision > 3
    ));
    assert_eq!(held(&patched), Some(&json!(after)));
    assert!(subscriptions
        .deliver_patch::<Snapshot>(99, 0, json!({}), None)
        .is_none());
}

#[test]
fn a_resync_names_what_its_snapshot_leaves_the_client_holding() {
    let mut subscriptions = subscriptions_of_a_client();
    let id = subscriptions.insert("notes.byId".to_string(), json!({}), 3);
    let state = "resynced".to_string();

    let (accepted, event) = subscriptions
        .resync(
            id,
            json!({ "note": "snapshot" }),
            Some(state.clone()),
            "clientRequested",
        )
        .expect("the subscription exists");

    assert_eq!(accepted.from_revision, 3);
    assert!(accepted.revision > accepted.from_revision);
    assert_eq!(held(&event), Some(&json!(state)));
    assert_eq!(
        subscriptions.diagnostics().last_resync_reason.as_deref(),
        Some("clientRequested")
    );
}

#[test]
fn diagnostics_count_resumes_resyncs_and_mutations_and_hash_params() {
    let mut subscriptions = subscriptions_of_a_client();
    let secret = "content that must not cross diagnostics";
    let state = "held".to_string();
    let (accepted, _) = subscriptions.accept_subscription(
        "notes.byId".to_string(),
        json!({ "note_id": "private-note", "body": secret }),
        Some(state.clone()),
        json!({}),
        Some(state),
    );
    subscriptions.record_dropped_updates(3);
    let latencies = [Duration::from_micros(17), Duration::from_micros(29)];
    subscriptions.record_mutation(latencies[0], None);
    subscriptions.record_mutation(latencies[1], Some("stale_write"));

    let diagnostics = subscriptions.diagnostics();
    assert_eq!(diagnostics.subscriptions.len(), 1);
    assert_eq!(diagnostics.current_revision, accepted.revision);
    assert_eq!(diagnostics.resume_up_to_date_count, 1);
    assert_eq!(diagnostics.dropped_update_count, 3);
    assert_eq!(diagnostics.mutation_count, latencies.len() as u64);
    assert_eq!(diagnostics.mutation_rejection_count, 1);
    assert_eq!(
        diagnostics.mutation_latency_micros_total,
        latencies
            .iter()
            .map(|latency| latency.as_micros() as u64)
            .sum::<u64>()
    );
    assert_eq!(
        diagnostics.mutation_latency_micros_max,
        latencies
            .iter()
            .map(|latency| latency.as_micros() as u64)
            .max()
            .unwrap()
    );
    assert_eq!(diagnostics.rejections.get("stale_write"), Some(&1));
    assert_eq!(
        diagnostics.last_resync_reason.as_deref(),
        Some("broadcastLag")
    );

    let reported = format!("{diagnostics:?}");
    assert!(!reported.contains("private-note"));
    assert!(!reported.contains(secret));
    assert!(diagnostics.subscriptions[0]
        .params_sha256
        .starts_with("sha256:"));
}

#[test]
fn a_subscription_remembers_where_its_last_delivery_left_the_client() {
    let mut subscriptions = subscriptions_of_a_client();
    let id = subscriptions.insert("notes.authoringState".to_string(), json!({}), 3);
    assert_eq!(
        subscriptions.get(id).and_then(|s| s.delivered.clone()),
        None
    );

    subscriptions.record_delivery(id, 4, Some("snapshot-frontier".to_string()));
    let subscription = subscriptions.get(id).expect("subscribed");
    assert_eq!(subscription.revision, 4);
    assert_eq!(subscription.delivered.as_deref(), Some("snapshot-frontier"));
}

#[test]
fn a_delivery_built_on_a_superseded_one_is_not_sent() {
    let mut subscriptions = subscriptions_of_a_client();
    let id = subscriptions.insert("notes.authoringState".to_string(), json!({}), 3);
    let built_at = subscriptions.get(id).expect("subscribed").revision;

    // A resync delivers a snapshot while the patch is being built.
    subscriptions.record_delivery(id, built_at + 1, Some("resync-state".to_string()));
    assert_eq!(
        subscriptions
            .deliver_following::<Snapshot>(id, built_at, json!({}), "patch-state".to_string())
            .expect_err("superseded"),
        FollowingDelivery::Superseded
    );
    assert_eq!(
        subscriptions
            .get(id)
            .and_then(|s| s.delivered.clone())
            .as_deref(),
        Some("resync-state")
    );

    let current = subscriptions.get(id).expect("subscribed").revision;
    let next = "next-state".to_string();
    let event: Event = subscriptions
        .deliver_following(id, current, json!({}), next.clone())
        .expect("follows what the client holds");
    assert!(matches!(
        event,
        Event::Patch { from_revision, to_revision, .. }
            if from_revision == current && to_revision > current
    ));
    assert_eq!(held(&event), Some(&json!(next)));
    let subscription = subscriptions.get(id).expect("subscribed");
    assert_eq!(subscription.delivered.as_deref(), Some(next.as_str()));
    assert_eq!(
        subscriptions
            .deliver_following::<Snapshot>(99, 0, json!({}), "unknown".to_string())
            .expect_err("no such subscription"),
        FollowingDelivery::Superseded
    );
}

#[test]
fn a_delivery_that_leaves_the_client_where_the_last_one_did_is_not_sent() {
    let mut subscriptions = subscriptions_of_a_client();
    let id = subscriptions.insert("notes.authoringState".to_string(), json!({}), 3);
    subscriptions.record_delivery(id, 4, Some("held-state".to_string()));

    assert_eq!(
        subscriptions
            .deliver_following::<Snapshot>(id, 4, json!({}), "held-state".to_string())
            .expect_err("already held"),
        FollowingDelivery::AlreadyHeld
    );
    assert_eq!(subscriptions.get(id).expect("subscribed").revision, 4);
    assert!(subscriptions
        .deliver_following::<Snapshot>(id, 4, json!({}), "newer-state".to_string())
        .is_ok());
}

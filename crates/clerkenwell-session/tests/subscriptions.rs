use std::time::Duration;

use clerkenwell_session::transport::ProjectionSubscribeResume;
use clerkenwell_session::{CursorAhead, FollowingDelivery, ProjectionSubscriptions};
use serde_json::{json, Value};

type Patch = Value;
type Subscriptions = ProjectionSubscriptions<Patch, String>;

fn with_patches(projection: &str, params: &Value, spans: &[(u64, u64)]) -> Subscriptions {
    let mut subscriptions = Subscriptions::default();
    for (from, to) in spans {
        subscriptions.retain_patch(
            projection.to_string(),
            params.clone(),
            *from,
            *to,
            json!({ "to": to }),
        );
    }
    subscriptions
}

#[test]
fn retained_patches_replay_only_a_contiguous_window_for_the_same_projection_and_params() {
    let params = json!({});
    let subscriptions = with_patches("notes.list", &params, &[(1, 2), (2, 3)]);

    let retained = subscriptions
        .retained_patches("notes.list", &params, 1, 3)
        .expect("a contiguous matching window replays");
    let spans = retained
        .iter()
        .map(|patch| (patch.from_revision, patch.to_revision))
        .collect::<Vec<_>>();
    assert_eq!(spans, [(1, 2), (2, 3)]);

    assert!(subscriptions
        .retained_patches("notes.byId", &params, 1, 3)
        .is_none());
    assert!(subscriptions
        .retained_patches("notes.list", &json!({ "filter": true }), 1, 3)
        .is_none());
    assert!(subscriptions
        .retained_patches("notes.list", &params, 0, 3)
        .is_none());
}

#[test]
fn the_retained_window_evicts_the_oldest_patches() {
    let params = json!({});
    let mut subscriptions = Subscriptions::new(2);
    for revision in 1..=3 {
        subscriptions.retain_patch(
            "notes.list".to_string(),
            params.clone(),
            revision,
            revision + 1,
            json!(revision),
        );
    }
    assert!(subscriptions
        .retained_patches("notes.list", &params, 1, 4)
        .is_none());
    assert!(subscriptions
        .retained_patches("notes.list", &params, 2, 4)
        .is_some());
}

#[test]
fn the_last_retained_patch_is_matched_per_projection_and_params() {
    let params = json!({});
    let mut subscriptions = Subscriptions::default();
    let first = json!({ "kind": "reset" });
    let latest = json!({ "kind": "remove", "id": "n1" });
    subscriptions.retain_patch(
        "notes.list".to_string(),
        params.clone(),
        1,
        2,
        first.clone(),
    );
    subscriptions.retain_patch(
        "notes.byId".to_string(),
        params.clone(),
        2,
        3,
        first.clone(),
    );
    subscriptions.retain_patch(
        "notes.list".to_string(),
        params.clone(),
        3,
        4,
        latest.clone(),
    );

    assert!(subscriptions.last_retained_patch_matches("notes.list", &params, &latest));
    assert!(!subscriptions.last_retained_patch_matches("notes.list", &params, &first));
    assert!(!subscriptions.last_retained_patch_matches(
        "notes.list",
        &json!({ "filter": true }),
        &latest
    ));
}

#[test]
fn a_subscription_without_a_cursor_takes_a_snapshot() {
    let mut subscriptions = Subscriptions::default();
    let revision = 7;
    let outcome = subscriptions
        .subscribe("notes.list".to_string(), json!({}), revision, None)
        .expect("subscribed");
    assert_eq!(outcome.resume, None);
    assert!(outcome.replay.is_none());
    assert_eq!(
        subscriptions
            .get(outcome.subscription_id)
            .map(|subscription| subscription.revision),
        Some(revision)
    );
}

#[test]
fn a_cursor_ahead_of_the_projection_is_refused_without_subscribing() {
    let mut subscriptions = Subscriptions::default();
    let refused = subscriptions
        .subscribe("notes.list".to_string(), json!({}), 3, Some(4))
        .expect_err("a cursor from the future is refused");
    assert_eq!(
        refused,
        CursorAhead {
            cursor_revision: 4,
            current_revision: 3
        }
    );
    assert!(subscriptions.all().is_empty());
}

#[test]
fn a_cursor_at_the_current_revision_resumes_with_nothing_to_send() {
    let mut subscriptions = Subscriptions::default();
    let revision = 5;
    let outcome = subscriptions
        .subscribe(
            "notes.list".to_string(),
            json!({}),
            revision,
            Some(revision),
        )
        .expect("subscribed");
    assert_eq!(
        outcome.resume,
        Some(ProjectionSubscribeResume::UpToDate { revision })
    );
    assert_eq!(outcome.replay, Some(Vec::new()));
}

#[test]
fn a_cursor_inside_the_retained_window_resumes_with_the_missed_patches() {
    let params = json!({});
    let mut subscriptions = with_patches("notes.list", &params, &[(1, 2), (2, 3), (3, 4)]);
    let outcome = subscriptions
        .subscribe("notes.list".to_string(), params, 4, Some(2))
        .expect("subscribed");
    let replay = outcome.replay.expect("patches replay");
    assert_eq!(
        outcome.resume,
        Some(ProjectionSubscribeResume::Patches {
            from_revision: 2,
            revision: 4,
            patch_count: replay.len(),
        })
    );
    assert_eq!(replay.first().map(|patch| patch.from_revision), Some(2));
    assert_eq!(replay.last().map(|patch| patch.to_revision), Some(4));
}

#[test]
fn a_cursor_outside_the_retained_window_resumes_with_a_snapshot_and_counts_a_resync() {
    let params = json!({});
    let mut subscriptions = with_patches("notes.list", &params, &[(3, 4)]);
    let outcome = subscriptions
        .subscribe("notes.list".to_string(), params, 4, Some(1))
        .expect("subscribed");
    assert_eq!(
        outcome.resume,
        Some(ProjectionSubscribeResume::Snapshot {
            from_revision: 1,
            revision: 4
        })
    );
    assert!(outcome.replay.is_none());
    let diagnostics = subscriptions.diagnostics();
    assert_eq!(diagnostics.resume_snapshot_count, 1);
    assert_eq!(diagnostics.gapped_update_count, 1);
    assert_eq!(
        diagnostics.last_resync_reason.as_deref(),
        Some("retainedWindowExpired")
    );
}

#[test]
fn diagnostics_count_resumes_resyncs_and_mutations_and_hash_params() {
    let mut subscriptions = with_patches("notes.byId", &json!({}), &[]);
    let secret = "content that must not cross diagnostics";
    let revision = 11;
    subscriptions
        .subscribe(
            "notes.byId".to_string(),
            json!({ "note_id": "private-note", "body": secret }),
            revision,
            Some(revision),
        )
        .expect("subscribed");
    subscriptions.record_dropped_updates(3);
    let latencies = [Duration::from_micros(17), Duration::from_micros(29)];
    subscriptions.record_mutation(latencies[0], None);
    subscriptions.record_mutation(latencies[1], Some("stale_write"));

    let diagnostics = subscriptions.diagnostics();
    assert_eq!(diagnostics.subscriptions.len(), 1);
    assert_eq!(diagnostics.current_revision, revision);
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
    let mut subscriptions = Subscriptions::default();
    let id = subscriptions.insert("notes.authoringState".to_string(), json!({}), 3);
    assert_eq!(
        subscriptions.get(id).and_then(|s| s.delivered.clone()),
        None
    );

    subscriptions.record_delivery(id, 4, "snapshot-frontier".to_string());
    let subscription = subscriptions.get(id).expect("subscribed");
    assert_eq!(subscription.revision, 4);
    assert_eq!(subscription.delivered.as_deref(), Some("snapshot-frontier"));
}

#[test]
fn a_delivery_built_on_a_superseded_one_is_not_recorded() {
    let mut subscriptions = Subscriptions::default();
    let id = subscriptions.insert("notes.authoringState".to_string(), json!({}), 3);
    let built_at = subscriptions.get(id).expect("subscribed").revision;

    // A resync delivers a snapshot while the patch is being built.
    subscriptions.record_delivery(id, built_at + 1, "resync-state".to_string());
    assert_eq!(
        subscriptions.record_following_delivery(
            id,
            built_at,
            built_at + 1,
            "patch-state".to_string()
        ),
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
    assert_eq!(
        subscriptions.record_following_delivery(id, current, current + 1, "next-state".to_string()),
        FollowingDelivery::Recorded
    );
    let subscription = subscriptions.get(id).expect("subscribed");
    assert_eq!(subscription.revision, current + 1);
    assert_eq!(subscription.delivered.as_deref(), Some("next-state"));
    assert_eq!(
        subscriptions.record_following_delivery(99, 0, 1, "unknown".to_string()),
        FollowingDelivery::Superseded
    );
}

#[test]
fn a_delivery_that_leaves_the_client_where_the_last_one_did_is_not_recorded() {
    let mut subscriptions = Subscriptions::default();
    let id = subscriptions.insert("notes.authoringState".to_string(), json!({}), 3);
    subscriptions.record_delivery(id, 4, "held-state".to_string());

    assert_eq!(
        subscriptions.record_following_delivery(id, 4, 5, "held-state".to_string()),
        FollowingDelivery::AlreadyHeld
    );
    assert_eq!(subscriptions.get(id).expect("subscribed").revision, 4);
    assert_eq!(
        subscriptions.record_following_delivery(id, 4, 5, "newer-state".to_string()),
        FollowingDelivery::Recorded
    );
}

use std::sync::{Arc, Mutex};

use clerkenwell_events::{
    ChangeData, ChangeEvent, ChangeFeed, ChangeKind, DATA_CONTENT_TYPE, SPEC_VERSION,
};
use serde_json::Value;

fn event(resource_id: &str, generation: u64) -> ChangeEvent {
    ChangeEvent::new(
        format!("event-{resource_id}-{generation}"),
        "tasks-store",
        ChangeKind::Committed,
        "2026-09-29T12:00:00.000Z",
        Some(generation),
        ChangeData {
            entity: "Task".to_string(),
            resource_id: resource_id.to_string(),
            moved_from: None,
            etag_before: None,
            etag_after: Some("etag-after".to_string()),
            frontier_before: None,
            frontier_after: Some("frontier-after".to_string()),
            operation_id: Some("op-1".to_string()),
        },
    )
}

/// CloudEvents 1.0 attribute names: lowercase ASCII letters and digits.
fn is_cloud_events_attribute(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
}

#[test]
fn a_change_event_is_a_cloud_events_event() {
    let encoded = serde_json::to_value(event("task-1", 3)).expect("encode");
    let attributes = encoded.as_object().expect("an object");
    for required in ["specversion", "id", "source", "type", "subject", "time"] {
        assert!(
            attributes
                .get(required)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            "{required} is a non-empty string"
        );
    }
    assert_eq!(attributes["specversion"], SPEC_VERSION);
    assert_eq!(attributes["datacontenttype"], DATA_CONTENT_TYPE);
    for name in attributes.keys() {
        assert!(
            is_cloud_events_attribute(name),
            "{name} is a CloudEvents attribute name"
        );
    }
    for kind in [
        ChangeKind::Committed,
        ChangeKind::Moved,
        ChangeKind::Deleted,
    ] {
        let name = serde_json::to_value(kind).expect("encode kind");
        assert!(
            name.as_str()
                .is_some_and(|name| name.starts_with("clerkenwell.document.")),
            "{name} is a Clerkenwell document event type"
        );
    }
}

#[test]
fn a_change_event_names_its_document_and_leaves_out_what_the_commit_did_not_say() {
    let change = event("task-1", 3);
    assert_eq!(
        change.subject,
        format!("{}/{}", change.data.entity, change.data.resource_id)
    );
    let encoded = serde_json::to_value(&change).expect("encode");
    for absent in [
        "actorid",
        "actorkind",
        "intent",
        "proposalstate",
        "traceparent",
    ] {
        assert!(encoded.get(absent).is_none(), "{absent} is left out");
    }
    let decoded: ChangeEvent = serde_json::from_value(encoded).expect("decode");
    assert_eq!(decoded, change);
}

#[test]
fn every_listener_hears_every_announcement_in_order() {
    let feed = ChangeFeed::default();
    assert!(!feed.is_heard());
    let heard: Vec<Arc<Mutex<Vec<String>>>> = (0..2).map(|_| Arc::default()).collect();
    for log in &heard {
        let log = log.clone();
        feed.listen(move |change| log.lock().unwrap().push(change.id.clone()));
    }
    assert!(feed.is_heard());
    let announced: Vec<ChangeEvent> = (1..=3)
        .map(|generation| event("task-1", generation))
        .collect();
    for change in &announced {
        feed.announce(change);
    }
    let ids: Vec<String> = announced.iter().map(|change| change.id.clone()).collect();
    for log in heard {
        assert_eq!(*log.lock().unwrap(), ids);
    }
}

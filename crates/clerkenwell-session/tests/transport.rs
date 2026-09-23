use clerkenwell_session::transport::{
    ProjectionMutationAccepted, ProjectionMutationCommand, ProjectionSubscribeCommand,
    ProjectionSubscribeCursor, ProjectionSubscribeResume, ProjectionTransportEvent,
};
use serde_json::{json, Value};

#[test]
fn commands_round_trip_without_hidden_defaults() {
    let subscribe = ProjectionSubscribeCommand {
        projection: "notes.list".to_owned(),
        params: Some(json!({})),
        cursor: Some(ProjectionSubscribeCursor { revision: 42 }),
    };
    let subscribe_json = serde_json::to_value(&subscribe).expect("encode subscribe");
    assert_eq!(
        serde_json::from_value::<ProjectionSubscribeCommand>(subscribe_json)
            .expect("decode subscribe"),
        subscribe
    );

    let mutation = ProjectionMutationCommand {
        mutation: "note.delete".to_owned(),
        operation_id: Some("op-delete".to_owned()),
        base_revision: Some(42),
        params: json!({ "note_id": "note-1" }),
    };
    let mutation_json = serde_json::to_value(&mutation).expect("encode mutation");
    assert_eq!(
        serde_json::from_value::<ProjectionMutationCommand>(mutation_json)
            .expect("decode mutation"),
        mutation
    );
}

#[test]
fn accepted_mutations_and_events_carry_the_application_payload_intact() {
    let accepted = ProjectionMutationAccepted {
        operation_id: Some("op-delete".to_owned()),
        base_revision: Some(4),
        revision: 5,
        result: json!({ "deleted_note_id": "note-1" }),
    };
    let encoded = serde_json::to_value(&accepted).expect("encode accepted");
    assert_eq!(
        serde_json::from_value::<ProjectionMutationAccepted<Value>>(encoded)
            .expect("decode accepted"),
        accepted
    );

    for event in [
        ProjectionTransportEvent::Snapshot {
            subscription_id: 7,
            revision: 4,
            snapshot: json!({ "notes": [] }),
        },
        ProjectionTransportEvent::Patch {
            subscription_id: 7,
            from_revision: 4,
            to_revision: 5,
            patch: json!({ "kind": "remove", "note_id": "note-1" }),
        },
    ] {
        let encoded = serde_json::to_value(&event).expect("encode event");
        assert_eq!(
            serde_json::from_value::<ProjectionTransportEvent<Value, Value>>(encoded)
                .expect("decode event"),
            event
        );
    }
}

#[test]
fn resume_outcomes_are_explicit_and_bounded() {
    let outcomes = [
        ProjectionSubscribeResume::UpToDate { revision: 9 },
        ProjectionSubscribeResume::Patches {
            from_revision: 4,
            revision: 9,
            patch_count: 5,
        },
        ProjectionSubscribeResume::Snapshot {
            from_revision: 1,
            revision: 9,
        },
    ];
    for outcome in outcomes {
        let value = serde_json::to_value(&outcome).expect("encode resume");
        assert_eq!(
            serde_json::from_value::<ProjectionSubscribeResume>(value).expect("decode resume"),
            outcome
        );
    }
}

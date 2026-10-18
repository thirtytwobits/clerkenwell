use clerkenwell_session::transport::{
    ProjectionCommandAccepted, ProjectionCommandRequest, ProjectionMutationAccepted,
    ProjectionMutationRequest, ProjectionOperation, ProjectionSubscribeAccepted,
    ProjectionSubscribeRequest, ProjectionTransportEvent, PROJECTION_COMMAND_METHOD,
    PROJECTION_MUTATE_METHOD, PROJECTION_RESYNC_METHOD, PROJECTION_SUBSCRIBE_METHOD,
    PROJECTION_UNSUBSCRIBE_METHOD,
};
use serde_json::{json, Value};

#[test]
fn requests_round_trip_without_hidden_defaults() {
    let subscribe = ProjectionSubscribeRequest {
        projection: "notes.list".to_owned(),
        params: Some(json!({})),
        held: Some(json!({ "frontier_base64": "AAE=", "etag": "loro:1" })),
    };
    let subscribe_json = serde_json::to_value(&subscribe).expect("encode subscribe");
    assert_eq!(
        serde_json::from_value::<ProjectionSubscribeRequest>(subscribe_json)
            .expect("decode subscribe"),
        subscribe
    );

    let mutation = ProjectionMutationRequest {
        mutation: "note.delete".to_owned(),
        operation_id: Some("op-delete".to_owned()),
        base_revision: Some(42),
        params: json!({ "note_id": "note-1" }),
    };
    let mutation_json = serde_json::to_value(&mutation).expect("encode mutation");
    assert_eq!(
        serde_json::from_value::<ProjectionMutationRequest>(mutation_json)
            .expect("decode mutation"),
        mutation
    );

    for operation_id in [None, Some("op-archive".to_owned())] {
        let command = ProjectionCommandRequest {
            command: "board.archiveDone".to_owned(),
            operation_id: operation_id.clone(),
            params: json!({ "board_id": "board-1" }),
        };
        let command_json = serde_json::to_value(&command).expect("encode command");
        assert_eq!(
            command_json.get("operation_id").is_some(),
            operation_id.is_some()
        );
        assert_eq!(
            serde_json::from_value::<ProjectionCommandRequest>(command_json)
                .expect("decode command"),
            command
        );
    }
}

#[test]
fn a_command_request_names_its_params() {
    assert!(serde_json::from_value::<ProjectionCommandRequest>(
        json!({ "command": "board.archiveDone" })
    )
    .is_err());
    assert!(serde_json::from_value::<ProjectionCommandRequest>(json!({
        "command": "board.archiveDone",
        "params": {},
        "base_revision": 3,
    }))
    .is_err());
}

#[test]
fn every_protocol_method_names_its_own_operation() {
    let methods = [
        (PROJECTION_SUBSCRIBE_METHOD, ProjectionOperation::Subscribe),
        (PROJECTION_RESYNC_METHOD, ProjectionOperation::Resync),
        (
            PROJECTION_UNSUBSCRIBE_METHOD,
            ProjectionOperation::Unsubscribe,
        ),
        (PROJECTION_MUTATE_METHOD, ProjectionOperation::Mutate),
        (PROJECTION_COMMAND_METHOD, ProjectionOperation::Command),
    ];
    for (method, operation) in methods {
        assert_eq!(ProjectionOperation::for_method(method), Some(operation));
    }
    for (index, (method, _)) in methods.iter().enumerate() {
        assert!(methods[index + 1..]
            .iter()
            .all(|(other, _)| other != method));
    }
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

    let accepted = ProjectionCommandAccepted {
        operation_id: Some("op-archive".to_owned()),
        revision: 6,
        result: json!({ "command": "board.archiveDone", "value": { "archived": 2 } }),
    };
    let encoded = serde_json::to_value(&accepted).expect("encode accepted command");
    assert_eq!(
        serde_json::from_value::<ProjectionCommandAccepted<Value>>(encoded)
            .expect("decode accepted command"),
        accepted
    );

    for event in [
        ProjectionTransportEvent::Snapshot {
            subscription_id: 7,
            revision: 4,
            snapshot: json!({ "notes": [] }),
            held: Some(json!({ "etag": "loro:4" })),
        },
        ProjectionTransportEvent::Patch {
            subscription_id: 7,
            from_revision: 4,
            to_revision: 5,
            patch: json!({ "kind": "remove", "note_id": "note-1" }),
            held: None,
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
fn an_accepted_subscription_says_only_when_no_snapshot_follows() {
    for up_to_date in [false, true] {
        let accepted = ProjectionSubscribeAccepted {
            subscription_id: 3,
            revision: 9,
            up_to_date,
        };
        let encoded = serde_json::to_value(&accepted).expect("encode accepted");
        assert_eq!(encoded.get("up_to_date").is_some(), up_to_date);
        assert_eq!(
            serde_json::from_value::<ProjectionSubscribeAccepted>(encoded)
                .expect("decode accepted"),
            accepted
        );
    }
}

#[test]
fn a_refusal_envelope_encodes_as_the_client_reads_it() {
    use clerkenwell_session::transport::{
        ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionOperation,
    };

    for code in ProjectionErrorCode::ALL {
        let envelope = ProjectionErrorEnvelope::new(
            code,
            "Refused.",
            ProjectionOperation::Mutate,
            Some("note.delete"),
            None,
        );

        let encoded = serde_json::to_value(&envelope).expect("encode envelope");

        assert_eq!(encoded["code"], json!(code.as_str()));
        assert_eq!(ProjectionErrorCode::parse(code.as_str()), Some(code));
        assert_eq!(encoded["operation"], json!("mutate"));
        assert_eq!(encoded["retryable"], json!(code.retryable()));
        assert!(encoded.get("details").is_none());
        assert_eq!(
            serde_json::from_value::<ProjectionErrorEnvelope>(encoded).expect("decode envelope"),
            envelope
        );
    }
}

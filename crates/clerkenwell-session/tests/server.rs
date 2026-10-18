//! One connection's projection requests served against an application host.

use std::future::Future;
use std::pin::pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_schema::{
    GeneratedAuthoringPolicyKind, GeneratedCommandSpec, GeneratedEntityAuthoringSpec,
    GeneratedMaterializationPlan, GeneratedMutationSpec, GeneratedProjectionSpec,
};
use clerkenwell_session::transport::{
    ProjectionCommandAccepted, ProjectionCommandRequest, ProjectionErrorCode,
    ProjectionErrorEnvelope, ProjectionMutationAccepted, ProjectionMutationRequest,
    ProjectionOperation, ProjectionResyncAccepted, ProjectionResyncRequest,
    ProjectionSubscribeAccepted, ProjectionSubscribeRequest, ProjectionTransportEvent,
    ProjectionUnsubscribeAccepted, ProjectionUnsubscribeRequest, PROJECTION_COMMAND_METHOD,
    PROJECTION_MUTATE_METHOD, PROJECTION_SUBSCRIBE_METHOD,
};
use clerkenwell_session::{
    publish, resync_all, serve, serve_request, Accepted, Held, ProjectionFailure, ProjectionHost,
    ProjectionRefusal, ProjectionRegistry, ProjectionReply, ProjectionRequest,
    ProjectionSubscription, ProjectionSubscriptions,
};
use serde_json::{json, Value};

static PROJECTIONS: &[GeneratedProjectionSpec] = &[
    GeneratedProjectionSpec {
        name: "notes.list",
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
    GeneratedProjectionSpec {
        name: "notes.replica",
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
    GeneratedProjectionSpec {
        name: "boards.list",
        depends_on: &["Board"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
];
static MUTATIONS: &[GeneratedMutationSpec] = &[
    GeneratedMutationSpec {
        name: "note.rename",
        touches: &["Note"],
    },
    GeneratedMutationSpec {
        name: "note.preview",
        touches: &["Note"],
    },
    GeneratedMutationSpec {
        name: "board.pin",
        touches: &["Board"],
    },
];
static AUTHORING: &[GeneratedEntityAuthoringSpec] = &[GeneratedEntityAuthoringSpec {
    entity: "Note",
    kind: GeneratedAuthoringPolicyKind::CommandOwned,
    mutations: &["note.rename", "note.preview"],
    planning_mutations: &["note.preview"],
}];
static COMMANDS: &[GeneratedCommandSpec] = &[
    GeneratedCommandSpec {
        name: "note.archive",
        touches: &["Note"],
    },
    GeneratedCommandSpec {
        name: "note.count",
        touches: &[],
    },
];
static REGISTRY: ProjectionRegistry =
    ProjectionRegistry::new(PROJECTIONS, MUTATIONS, COMMANDS, AUTHORING);

#[derive(Debug)]
struct Failure {
    refusal: ProjectionRefusal,
    envelope: Option<Box<ProjectionErrorEnvelope>>,
}

impl ProjectionFailure for Failure {
    fn refused(refusal: ProjectionRefusal) -> Self {
        Self {
            refusal,
            envelope: None,
        }
    }

    fn describe(&self) -> ProjectionRefusal {
        self.refusal.clone()
    }

    fn described(mut self, envelope: ProjectionErrorEnvelope) -> Self {
        self.envelope = Some(Box::new(envelope));
        self
    }
}

/// A note, its title and the frontier its authoring state names.
struct Notes {
    subscriptions: Mutex<ProjectionSubscriptions<Value, Held<String>>>,
    title: Mutex<String>,
    refusal: Option<ProjectionRefusal>,
    mutations_run: Mutex<usize>,
    /// The principal each mutation was made as, in order.
    mutated_by: Mutex<Vec<Principal>>,
    /// The principal each command was run as, in order.
    commanded_by: Mutex<Vec<Principal>>,
    /// The projection of each subscription the session asked a command's
    /// patch for, in order.
    command_patches_asked: Mutex<Vec<String>>,
}

/// The connection the host authenticated as `id`.
fn connection(id: &str) -> Mutex<ProjectionSubscriptions<Value, Held<String>>> {
    Mutex::new(ProjectionSubscriptions::new(Principal::new(
        id,
        ActorKind::Human,
    )))
}

impl Notes {
    fn new() -> Self {
        Self {
            subscriptions: connection("reader"),
            title: Mutex::new("Shopping".to_owned()),
            refusal: None,
            mutations_run: Mutex::new(0),
            mutated_by: Mutex::new(Vec::new()),
            commanded_by: Mutex::new(Vec::new()),
            command_patches_asked: Mutex::new(Vec::new()),
        }
    }

    fn command_patches_asked(&self) -> Vec<String> {
        self.command_patches_asked
            .lock()
            .expect("command patches")
            .clone()
    }

    fn title(&self) -> String {
        self.title.lock().expect("title").clone()
    }

    fn subscription(&self, subscription_id: u64) -> ProjectionSubscription<Held<String>> {
        self.subscriptions
            .lock()
            .expect("subscriptions")
            .get(subscription_id)
            .cloned()
            .expect("the subscription")
    }
}

impl ProjectionHost for Notes {
    type Snapshot = Value;
    type Patch = Value;
    type Delivery = String;
    type MutationResult = Value;
    type CommandResult = Value;
    type Failure = Failure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    fn snapshot(
        &self,
        projection: &str,
        _params: &Value,
        _held: Option<&String>,
    ) -> impl Future<Output = Result<Value, Failure>> + Send {
        let snapshot = match projection {
            "notes.replica" => json!({ "frontier": format!("frontier:{}", self.title()) }),
            "boards.list" => json!({ "pinned": false }),
            _ => json!({ "title": self.title() }),
        };
        async move { Ok(snapshot) }
    }

    fn delivered(&self, snapshot: &Value) -> Option<String> {
        snapshot["frontier"].as_str().map(str::to_owned)
    }

    fn delivered_by_patch(&self, patch: &Value) -> Option<String> {
        patch["frontier"].as_str().map(str::to_owned)
    }

    fn mutate(
        &self,
        principal: &Principal,
        mutation: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, Failure>> + Send {
        *self.mutations_run.lock().expect("count") += 1;
        self.mutated_by
            .lock()
            .expect("principals")
            .push(principal.clone());
        let outcome = match (&self.refusal, mutation) {
            (Some(refusal), _) => Err(Failure::refused(refusal.clone())),
            (None, "note.rename") => {
                let title = params["title"].as_str().expect("a title").to_owned();
                *self.title.lock().expect("title") = title.clone();
                Ok(json!({ "title": title }))
            }
            (None, "note.preview") => Ok(json!({ "title": params["title"] })),
            (None, _) => Ok(json!({})),
        };
        async move { outcome }
    }

    fn mutation_patch(
        &self,
        _mutation: &str,
        result: &Value,
        subscription: &ProjectionSubscription<Held<String>>,
    ) -> impl Future<Output = Option<Value>> + Send {
        // The authoring state reaches its subscribers another way.
        let patch = match subscription.projection.as_str() {
            "notes.list" => Some(json!({ "title": result["title"] })),
            "boards.list" => Some(json!({ "pinned": true })),
            _ => None,
        };
        async move { patch }
    }

    fn command(
        &self,
        principal: &Principal,
        command: &str,
        _params: Value,
    ) -> impl Future<Output = Result<Value, Failure>> + Send {
        self.commanded_by
            .lock()
            .expect("principals")
            .push(principal.clone());
        let outcome = match (&self.refusal, command) {
            (Some(refusal), _) => Err(Failure::refused(refusal.clone())),
            (None, "note.archive") => Ok(json!({ "archived": self.title() })),
            (None, _) => Ok(json!({ "words": self.title().split_whitespace().count() })),
        };
        async move { outcome }
    }

    fn command_patch(
        &self,
        _command: &str,
        result: &Value,
        subscription: &ProjectionSubscription<Held<String>>,
    ) -> impl Future<Output = Option<Value>> + Send {
        self.command_patches_asked
            .lock()
            .expect("command patches")
            .push(subscription.projection.clone());
        // The authoring state reaches its subscribers another way.
        let patch = match subscription.projection.as_str() {
            "notes.list" => Some(json!({ "archived": result["archived"] })),
            _ => None,
        };
        async move { patch }
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

type Reply = ProjectionReply<Value, Value, Value, Value>;

fn subscribe(
    host: &Notes,
    projection: &str,
    held: Option<Value>,
) -> (ProjectionSubscribeAccepted, Reply) {
    let reply = block_on(serve(
        host,
        &host.subscriptions,
        ProjectionRequest::Subscribe(ProjectionSubscribeRequest {
            projection: projection.to_owned(),
            params: Some(json!({})),
            held,
        }),
    ))
    .expect("subscribe");
    (
        serde_json::from_value(reply.result.clone()).expect("an accepted subscription"),
        reply,
    )
}

fn mutate(
    host: &Notes,
    mutation: &str,
    params: Value,
) -> Result<(ProjectionMutationAccepted<Value>, Reply), Failure> {
    let reply = block_on(serve(
        host,
        &host.subscriptions,
        ProjectionRequest::Mutate(ProjectionMutationRequest {
            mutation: mutation.to_owned(),
            operation_id: Some(format!("{mutation}-op")),
            base_revision: None,
            params,
        }),
    ))?;
    Ok((
        serde_json::from_value(reply.result.clone()).expect("an accepted mutation"),
        reply,
    ))
}

fn run(
    host: &Notes,
    connection: &Mutex<ProjectionSubscriptions<Value, Held<String>>>,
    command: &str,
) -> Result<(ProjectionCommandAccepted<Value>, Reply), Failure> {
    let reply = block_on(serve(
        host,
        connection,
        ProjectionRequest::Command(ProjectionCommandRequest {
            command: command.to_owned(),
            operation_id: Some(format!("{command}-op")),
            params: json!({}),
        }),
    ))?;
    Ok((
        serde_json::from_value(reply.result.clone()).expect("an accepted command"),
        reply,
    ))
}

fn rename(host: &Notes, title: &str) -> (ProjectionMutationAccepted<Value>, Reply) {
    mutate(host, "note.rename", json!({ "title": title })).expect("rename")
}

fn envelope(failure: &Failure) -> &ProjectionErrorEnvelope {
    failure
        .envelope
        .as_ref()
        .expect("a failure reaches its client described")
}

/// What `event` says its client holds.
fn held_by(event: &ProjectionTransportEvent<Value, Value>) -> Option<Value> {
    match event {
        ProjectionTransportEvent::Snapshot { held, .. }
        | ProjectionTransportEvent::Patch { held, .. } => held.clone(),
    }
}

fn patch_spans(events: &[ProjectionTransportEvent<Value, Value>]) -> Vec<(u64, u64, u64)> {
    events
        .iter()
        .map(|event| match event {
            ProjectionTransportEvent::Patch {
                subscription_id,
                from_revision,
                to_revision,
                ..
            } => (*subscription_id, *from_revision, *to_revision),
            ProjectionTransportEvent::Snapshot { .. } => {
                panic!("expected only patches: {events:?}")
            }
        })
        .collect()
}

#[test]
fn a_subscription_receives_a_snapshot_at_the_revision_it_is_accepted_at() {
    let host = Notes::new();

    let (accepted, reply) = subscribe(&host, "notes.list", None);

    assert!(accepted.revision >= 1);
    assert!(!accepted.up_to_date);
    assert_eq!(
        reply.events,
        [ProjectionTransportEvent::Snapshot {
            subscription_id: accepted.subscription_id,
            revision: accepted.revision,
            snapshot: json!({ "title": host.title() }),
            held: None,
        }]
    );
    assert_eq!(
        host.subscription(accepted.subscription_id).revision,
        accepted.revision
    );
}

#[test]
fn what_a_snapshot_leaves_its_client_holding_is_recorded() {
    let host = Notes::new();

    let (accepted, reply) = subscribe(&host, "notes.replica", None);

    let ProjectionTransportEvent::Snapshot { snapshot, .. } = &reply.events[0] else {
        panic!("a snapshot: {:?}", reply.events);
    };
    assert_eq!(
        host.subscription(accepted.subscription_id).delivered,
        host.delivered(snapshot).map(Held::Host)
    );
}

#[test]
fn a_mutation_patches_every_subscription_it_affects_and_no_other() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let (_boards, _) = subscribe(&host, "boards.list", None);

    let (accepted, reply) = rename(&host, "Errands");

    assert_eq!(accepted.operation_id.as_deref(), Some("note.rename-op"));
    assert!(accepted.revision > notes.revision);
    assert_eq!(
        patch_spans(&reply.events),
        [(notes.subscription_id, notes.revision, accepted.revision)]
    );
    assert_eq!(
        host.subscription(notes.subscription_id).revision,
        accepted.revision
    );
}

#[test]
fn a_subscription_whose_changes_arrive_another_way_takes_no_mutation_patch() {
    let host = Notes::new();
    let (authoring, _) = subscribe(&host, "notes.replica", None);

    let (_, reply) = rename(&host, "Errands");

    assert!(reply.events.is_empty(), "{:?}", reply.events);
    assert_eq!(
        host.subscription(authoring.subscription_id).revision,
        authoring.revision
    );
}

#[test]
fn a_subscriber_that_holds_the_current_state_is_sent_nothing() {
    let host = Notes::new();
    let (_, first) = subscribe(&host, "notes.replica", None);
    let held = held_by(&first.events[0]).expect("the snapshot names what it leaves held");

    let (resumed, reply) = subscribe(&host, "notes.replica", Some(held));

    assert!(resumed.up_to_date);
    assert!(reply.events.is_empty(), "{:?}", reply.events);
    assert_eq!(
        host.subscription(resumed.subscription_id).delivered,
        Some(Held::Host(format!("frontier:{}", host.title())))
    );
}

#[test]
fn a_subscriber_that_holds_an_earlier_state_is_sent_a_snapshot() {
    let host = Notes::new();
    let (_, first) = subscribe(&host, "notes.replica", None);
    let held = held_by(&first.events[0]).expect("the snapshot names what it leaves held");
    rename(&host, "Errands");

    let (resumed, reply) = subscribe(&host, "notes.replica", Some(held));

    assert!(!resumed.up_to_date);
    let [event] = reply.events.as_slice() else {
        panic!("one snapshot: {:?}", reply.events);
    };
    assert!(matches!(event, ProjectionTransportEvent::Snapshot { .. }));
    assert_eq!(
        held_by(event),
        Some(json!(format!("frontier:{}", host.title())))
    );
}

#[test]
fn a_held_state_the_host_cannot_read_counts_as_holding_nothing() {
    let host = Notes::new();

    let (resumed, reply) = subscribe(&host, "notes.replica", Some(json!(42)));

    assert!(!resumed.up_to_date);
    assert_eq!(reply.events.len(), 1);
}

#[test]
fn an_unknown_projection_is_refused_before_the_host_builds_anything() {
    let host = Notes::new();

    let failure = block_on(serve(
        &host,
        &host.subscriptions,
        ProjectionRequest::Subscribe(ProjectionSubscribeRequest {
            projection: "notes.missing".to_owned(),
            params: None,
            held: None,
        }),
    ))
    .expect_err("an unknown projection is refused");

    assert_eq!(
        envelope(&failure).code,
        ProjectionErrorCode::UnknownProjection
    );
    assert!(host
        .subscriptions
        .lock()
        .expect("subscriptions")
        .all()
        .is_empty());
}

#[test]
fn a_resync_sends_a_snapshot_past_the_revision_the_client_holds() {
    let host = Notes::new();
    let (accepted, _) = subscribe(&host, "notes.list", None);

    let reply = block_on(serve(
        &host,
        &host.subscriptions,
        ProjectionRequest::Resync(ProjectionResyncRequest {
            subscription_id: accepted.subscription_id,
        }),
    ))
    .expect("resync");

    let resynced: ProjectionResyncAccepted =
        serde_json::from_value(reply.result).expect("an accepted resync");
    assert_eq!(resynced.from_revision, accepted.revision);
    assert!(resynced.revision > resynced.from_revision);
    assert_eq!(
        reply.events,
        [ProjectionTransportEvent::Snapshot {
            subscription_id: accepted.subscription_id,
            revision: resynced.revision,
            snapshot: json!({ "title": host.title() }),
            held: None,
        }]
    );
}

#[test]
fn resyncing_a_subscription_that_does_not_exist_is_refused_as_not_found() {
    let host = Notes::new();

    let failure = block_on(serve(
        &host,
        &host.subscriptions,
        ProjectionRequest::Resync(ProjectionResyncRequest { subscription_id: 9 }),
    ))
    .expect_err("no such subscription");

    let envelope = envelope(&failure);
    assert_eq!(envelope.code, ProjectionErrorCode::NotFound);
    assert_eq!(envelope.operation, ProjectionOperation::Resync);
}

#[test]
fn unsubscribing_reports_whether_the_subscription_was_there() {
    let host = Notes::new();
    let (accepted, _) = subscribe(&host, "notes.list", None);
    let unsubscribe = |subscription_id| {
        let reply = block_on(serve(
            &host,
            &host.subscriptions,
            ProjectionRequest::Unsubscribe(ProjectionUnsubscribeRequest { subscription_id }),
        ))
        .expect("unsubscribe");
        serde_json::from_value::<ProjectionUnsubscribeAccepted>(reply.result)
            .expect("an accepted unsubscribe")
            .removed
    };

    assert!(unsubscribe(accepted.subscription_id));
    assert!(!unsubscribe(accepted.subscription_id));
}

#[test]
fn an_unknown_mutation_is_refused_before_the_host_runs_anything() {
    let host = Notes::new();

    let failure = mutate(&host, "note.explode", json!({})).expect_err("an unknown mutation");

    let envelope = envelope(&failure);
    assert_eq!(envelope.code, ProjectionErrorCode::UnknownMutation);
    assert_eq!(envelope.operation, ProjectionOperation::Mutate);
    assert_eq!(envelope.name.as_deref(), Some("note.explode"));
    assert_eq!(*host.mutations_run.lock().expect("count"), 0);
}

#[test]
fn a_host_failure_reaches_its_client_described_by_its_code() {
    for code in ProjectionErrorCode::ALL {
        let details = json!({ "reason": "held" });
        let host = Notes {
            refusal: Some(ProjectionRefusal::new(
                code,
                "Refused.",
                Some(details.clone()),
            )),
            ..Notes::new()
        };

        let failure = mutate(&host, "note.rename", json!({ "title": "Errands" }))
            .expect_err("the host refuses");

        let envelope = envelope(&failure);
        assert_eq!(envelope.code, code);
        assert_eq!(envelope.retryable, code.retryable());
        assert_eq!(envelope.details.as_ref(), Some(&details));
    }
}

#[test]
fn only_a_rate_limit_or_a_base_revision_in_the_future_may_be_sent_again() {
    let retryable = ProjectionErrorCode::ALL
        .into_iter()
        .filter(|code| code.retryable())
        .collect::<Vec<_>>();

    assert_eq!(
        retryable,
        [
            ProjectionErrorCode::BaseRevisionInFuture,
            ProjectionErrorCode::RateLimit
        ]
    );
}

#[test]
fn a_planning_mutation_advances_no_revision_and_sends_nothing() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);

    let (accepted, reply) =
        mutate(&host, "note.preview", json!({ "title": "Maybe" })).expect("preview");

    assert_eq!(accepted.revision, notes.revision);
    assert!(reply.events.is_empty());
}

#[test]
fn mutations_and_their_refusals_are_counted() {
    let host = Notes::new();
    rename(&host, "Errands");
    mutate(&host, "note.explode", json!({})).expect_err("an unknown mutation");

    let diagnostics = host
        .subscriptions
        .lock()
        .expect("subscriptions")
        .diagnostics();

    assert_eq!(diagnostics.mutation_count, 2);
    assert_eq!(diagnostics.mutation_rejection_count, 1);
    assert_eq!(
        diagnostics
            .rejections
            .get(ProjectionErrorCode::UnknownMutation.as_str()),
        Some(&1)
    );
}

#[test]
fn a_patch_starts_from_the_revision_its_client_holds_when_it_is_delivered() {
    let mut subscriptions = ProjectionSubscriptions::<Value, Held<String>>::new(Principal::new(
        "other",
        ActorKind::Human,
    ));
    let subscription_id = subscriptions.insert("notes.list".to_owned(), json!({}), 5);

    // A delivery reached the client after the mutation chose its revision.
    let event = subscriptions
        .deliver_patch::<Value>(subscription_id, 3, json!({ "title": "Errands" }), None)
        .expect("the subscription is there");

    let ProjectionTransportEvent::Patch {
        from_revision,
        to_revision,
        ..
    } = event
    else {
        panic!("a patch: {event:?}");
    };
    assert_eq!(from_revision, 5);
    assert!(to_revision > from_revision);
    assert_eq!(
        subscriptions.get(subscription_id).map(|s| s.revision),
        Some(to_revision)
    );
}

#[test]
fn only_the_protocols_methods_decode_as_requests() {
    assert!(ProjectionRequest::decode("session.list", Some(json!({}))).is_none());

    let decoded = ProjectionRequest::decode(
        PROJECTION_SUBSCRIBE_METHOD,
        Some(json!({ "projection": "notes.list" })),
    )
    .expect("a protocol method")
    .expect("valid params");
    assert_eq!(decoded.operation(), ProjectionOperation::Subscribe);
    assert_eq!(decoded.name(), Some("notes.list"));
}

#[test]
fn a_request_without_valid_params_is_refused_as_invalid_params() {
    for params in [None, Some(json!({ "mutation": 7 }))] {
        let refusal = ProjectionRequest::decode(PROJECTION_MUTATE_METHOD, params)
            .expect("a protocol method")
            .expect_err("invalid params");
        assert_eq!(refusal.code, ProjectionErrorCode::InvalidParams);
    }
}

#[test]
fn a_request_outside_the_protocol_is_left_to_the_transport() {
    let host = Notes::new();

    assert!(block_on(serve_request(
        &host,
        &host.subscriptions,
        "session.list",
        Some(json!({}))
    ))
    .is_none());
}

#[test]
fn a_request_whose_params_do_not_decode_is_refused_addressed_to_its_operation() {
    let host = Notes::new();

    let failure = block_on(serve_request(
        &host,
        &host.subscriptions,
        PROJECTION_MUTATE_METHOD,
        None,
    ))
    .expect("a protocol method")
    .expect_err("missing params");

    let envelope = envelope(&failure);
    assert_eq!(envelope.code, ProjectionErrorCode::InvalidParams);
    assert_eq!(envelope.operation, ProjectionOperation::Mutate);
}

#[test]
fn a_method_is_served_as_the_request_it_names() {
    let host = Notes::new();

    let reply = block_on(serve_request(
        &host,
        &host.subscriptions,
        PROJECTION_SUBSCRIBE_METHOD,
        Some(json!({ "projection": "notes.list" })),
    ))
    .expect("a protocol method")
    .expect("subscribe");

    let accepted: ProjectionSubscribeAccepted =
        serde_json::from_value(reply.result).expect("an accepted subscription");
    assert!(host
        .subscriptions
        .lock()
        .expect("subscriptions")
        .get(accepted.subscription_id)
        .is_some());
}

#[test]
fn a_mutation_accepted_elsewhere_patches_the_subscriptions_it_affects() {
    let host = Notes::new();
    let elsewhere = connection("other");
    let (notes, _) = subscribe(&host, "notes.list", None);
    let reply = block_on(serve(
        &host,
        &elsewhere,
        ProjectionRequest::Mutate(ProjectionMutationRequest {
            mutation: "note.rename".to_owned(),
            operation_id: None,
            base_revision: None,
            params: json!({ "title": "Errands" }),
        }),
    ))
    .expect("rename elsewhere");
    assert_eq!(
        *host.mutated_by.lock().expect("principals"),
        [Principal::new("other", ActorKind::Human)],
        "the host makes a mutation as the connection that sent it"
    );
    let accepted = reply
        .accepted
        .expect("the reply names the mutation it accepted");

    let (revision, events) = block_on(publish(&host, &host.subscriptions, &accepted));

    assert_eq!(
        patch_spans(&events),
        [(notes.subscription_id, notes.revision, revision)]
    );
    assert_eq!(host.subscription(notes.subscription_id).revision, revision);
}

#[test]
fn only_a_mutation_or_a_command_is_offered_to_other_connections() {
    let host = Notes::new();

    let (_, subscribed) = subscribe(&host, "notes.list", None);
    let (_, renamed) = rename(&host, "Errands");
    let (counted, count) = run(&host, &host.subscriptions, "note.count").expect("count");

    assert!(subscribed.accepted.is_none());
    let Some(Accepted::Mutation { mutation, result }) = renamed.accepted else {
        panic!("an accepted mutation: {:?}", renamed.accepted);
    };
    assert_eq!(mutation, "note.rename");
    assert_eq!(result, json!({ "title": "Errands" }));
    let Some(Accepted::Command { command, result }) = count.accepted else {
        panic!("a served command: {:?}", count.accepted);
    };
    assert_eq!(command, "note.count");
    assert_eq!(
        result, counted.result,
        "the reply carries the host's result"
    );
}

#[test]
fn a_command_patches_every_subscription_reading_an_entity_it_touches_and_no_other() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let (_boards, _) = subscribe(&host, "boards.list", None);

    let (accepted, reply) = run(&host, &host.subscriptions, "note.archive").expect("archive");

    assert_eq!(accepted.operation_id.as_deref(), Some("note.archive-op"));
    assert!(accepted.revision > notes.revision);
    assert_eq!(
        patch_spans(&reply.events),
        [(notes.subscription_id, notes.revision, accepted.revision)]
    );
    assert_eq!(host.command_patches_asked(), ["notes.list"]);
    assert_eq!(
        host.subscription(notes.subscription_id).revision,
        accepted.revision
    );
}

#[test]
fn a_command_that_touches_nothing_patches_nothing_and_leaves_the_revision() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);

    let (accepted, reply) = run(&host, &host.subscriptions, "note.count").expect("count");

    assert!(reply.events.is_empty(), "{:?}", reply.events);
    assert_eq!(accepted.revision, notes.revision);
    assert!(host.command_patches_asked().is_empty());
}

#[test]
fn a_command_run_elsewhere_patches_the_subscriptions_it_affects() {
    let host = Notes::new();
    let elsewhere = connection("other");
    let (notes, _) = subscribe(&host, "notes.list", None);
    let (_, reply) = run(&host, &elsewhere, "note.archive").expect("archive elsewhere");
    assert_eq!(
        *host.commanded_by.lock().expect("principals"),
        [Principal::new("other", ActorKind::Human)],
        "the host runs a command as the connection that sent it"
    );
    let accepted = reply.accepted.expect("the reply names the command it ran");

    let (revision, events) = block_on(publish(&host, &host.subscriptions, &accepted));

    assert_eq!(
        patch_spans(&events),
        [(notes.subscription_id, notes.revision, revision)]
    );
    assert_eq!(host.subscription(notes.subscription_id).revision, revision);
}

#[test]
fn an_unknown_command_is_refused_before_the_host_runs_anything() {
    let host = Notes::new();

    for unknown in ["note.explode", "note.rename", "notes.list"] {
        let failure = run(&host, &host.subscriptions, unknown).expect_err("an unknown command");

        let envelope = envelope(&failure);
        assert_eq!(envelope.code, ProjectionErrorCode::UnknownCommand);
        assert_eq!(envelope.operation, ProjectionOperation::Command);
        assert_eq!(envelope.name.as_deref(), Some(unknown));
    }
    assert!(host.commanded_by.lock().expect("principals").is_empty());
    assert_eq!(*host.mutations_run.lock().expect("count"), 0);
}

#[test]
fn a_command_the_host_refuses_reaches_its_client_addressed_to_the_command() {
    let refusal = ProjectionRefusal::new(
        ProjectionErrorCode::ValidationFailed,
        "The note cannot be archived.",
        Some(json!({ "reason": "pinned" })),
    );
    let host = Notes {
        refusal: Some(refusal.clone()),
        ..Notes::new()
    };
    let (notes, _) = subscribe(&host, "notes.list", None);

    let failure = run(&host, &host.subscriptions, "note.archive").expect_err("refused");

    let envelope = envelope(&failure);
    assert_eq!(envelope.code, refusal.code);
    assert_eq!(envelope.message, refusal.message);
    assert_eq!(envelope.details, refusal.details);
    assert_eq!(envelope.operation, ProjectionOperation::Command);
    assert_eq!(envelope.name.as_deref(), Some("note.archive"));
    assert!(host.command_patches_asked().is_empty());
    assert_eq!(
        host.subscription(notes.subscription_id).revision,
        notes.revision
    );
}

#[test]
fn a_command_request_decodes_as_the_command_it_names() {
    let decoded = ProjectionRequest::decode(
        PROJECTION_COMMAND_METHOD,
        Some(json!({ "command": "note.count", "params": {} })),
    )
    .expect("a protocol method")
    .expect("valid params");

    assert_eq!(decoded.operation(), ProjectionOperation::Command);
    assert_eq!(decoded.name(), Some("note.count"));
    for params in [
        None,
        Some(json!({ "command": 7, "params": {} })),
        Some(json!({ "command": "note.count" })),
    ] {
        let refusal = ProjectionRequest::decode(PROJECTION_COMMAND_METHOD, params)
            .expect("a protocol method")
            .expect_err("invalid params");
        assert_eq!(refusal.code, ProjectionErrorCode::InvalidParams);
    }
}

#[test]
fn resyncing_a_connection_sends_every_subscription_a_snapshot() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let (boards, _) = subscribe(&host, "boards.list", None);

    let events = block_on(resync_all(&host, &host.subscriptions, "broadcastLag"));

    let resynced = events
        .iter()
        .map(|event| match event {
            ProjectionTransportEvent::Snapshot {
                subscription_id,
                revision,
                ..
            } => (*subscription_id, *revision),
            other => panic!("a snapshot: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(resynced.len(), 2);
    for (accepted, (subscription_id, revision)) in [notes, boards].iter().zip(resynced) {
        assert_eq!(subscription_id, accepted.subscription_id);
        assert!(revision > accepted.revision);
    }
    let diagnostics = host
        .subscriptions
        .lock()
        .expect("subscriptions")
        .diagnostics();
    assert_eq!(
        diagnostics.last_resync_reason.as_deref(),
        Some("broadcastLag")
    );
}

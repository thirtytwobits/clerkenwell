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
    command_reply, decode_request, deliver_announcement, publish, publish_command, resync_all,
    run_command, serve, serve_request, Accepted, Announced, Held, ModificationCounter, Modified,
    NumberedSnapshot, ProjectionFailure, ProjectionHost, ProjectionRefusal, ProjectionRegistry,
    ProjectionReply, ProjectionRequest, ProjectionSubscription, ProjectionSubscriptions,
    RanCommand,
};
use serde_json::{json, Value};

/// The schema of every parameter, result and snapshot here.
const OBJECT_SCHEMA: &str = r#"{"type":"object"}"#;

static PROJECTIONS: &[GeneratedProjectionSpec] = &[
    GeneratedProjectionSpec {
        name: "notes.list",
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
        params_schema: OBJECT_SCHEMA,
        snapshot_schema: OBJECT_SCHEMA,
    },
    GeneratedProjectionSpec {
        name: "notes.replica",
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
        params_schema: OBJECT_SCHEMA,
        snapshot_schema: OBJECT_SCHEMA,
    },
    GeneratedProjectionSpec {
        name: "boards.list",
        depends_on: &["Board"],
        materialization: GeneratedMaterializationPlan::Reset,
        params_schema: OBJECT_SCHEMA,
        snapshot_schema: OBJECT_SCHEMA,
    },
];
static MUTATIONS: &[GeneratedMutationSpec] = &[
    GeneratedMutationSpec {
        name: "note.rename",
        touches: &["Note"],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
    },
    GeneratedMutationSpec {
        name: "note.preview",
        touches: &["Note"],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
    },
    GeneratedMutationSpec {
        name: "board.pin",
        touches: &["Board"],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
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
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
    },
    GeneratedCommandSpec {
        name: "note.count",
        touches: &[],
        params_schema: OBJECT_SCHEMA,
        result_schema: OBJECT_SCHEMA,
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

/// A note, its title and the frontier its authoring state names. The title's
/// lock is the host's: it is held while a modification is numbered and saved,
/// and while a snapshot is read.
struct Notes {
    subscriptions: Mutex<ProjectionSubscriptions<Value, Held<String>>>,
    title: Mutex<String>,
    modifications: ModificationCounter,
    refusal: Option<ProjectionRefusal>,
    mutations_run: Mutex<usize>,
    /// The principal each mutation was made as, in order.
    mutated_by: Mutex<Vec<Principal>>,
    /// The principal each command was run as, in order.
    commanded_by: Mutex<Vec<Principal>>,
    /// The projection of each subscription the session asked a command's
    /// patch for, in order.
    command_patches_asked: Mutex<Vec<String>>,
    /// The projection of each subscription the session asked an
    /// announcement's patch for, in order.
    announcement_patches_asked: Mutex<Vec<String>>,
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
            modifications: ModificationCounter::default(),
            refusal: None,
            mutations_run: Mutex::new(0),
            mutated_by: Mutex::new(Vec::new()),
            commanded_by: Mutex::new(Vec::new()),
            command_patches_asked: Mutex::new(Vec::new()),
            announcement_patches_asked: Mutex::new(Vec::new()),
        }
    }

    fn announcement_patches_asked(&self) -> Vec<String> {
        self.announcement_patches_asked
            .lock()
            .expect("announcement patches")
            .clone()
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

    /// Saves `title` outside any request, as an announced change is saved,
    /// and returns the number of the modification.
    fn save(&self, title: &str) -> u64 {
        let mut held = self.title.lock().expect("title");
        let modification = self.modifications.take();
        *held = title.to_owned();
        modification
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
    type Announcement = Value;
    type Failure = Failure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    fn modifications(&self) -> &ModificationCounter {
        &self.modifications
    }

    fn snapshot(
        &self,
        projection: &str,
        _params: &Value,
        _held: Option<&String>,
    ) -> impl Future<Output = Result<NumberedSnapshot<Value>, Failure>> + Send {
        let title = self.title.lock().expect("title");
        let through = self.modifications.last();
        let snapshot = match projection {
            "notes.replica" => json!({ "frontier": format!("frontier:{}", *title) }),
            "boards.list" => json!({ "pinned": false }),
            _ => json!({ "title": *title }),
        };
        drop(title);
        async move { Ok(NumberedSnapshot { snapshot, through }) }
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
    ) -> impl Future<Output = Result<Modified<Value>, Failure>> + Send {
        *self.mutations_run.lock().expect("count") += 1;
        self.mutated_by
            .lock()
            .expect("principals")
            .push(principal.clone());
        let outcome = match (&self.refusal, mutation) {
            (Some(refusal), _) => Err(Failure::refused(refusal.clone())),
            (None, "note.rename") => {
                let title = params["title"].as_str().expect("a title");
                let modification = self.save(title);
                Ok(Modified {
                    result: json!({ "title": title }),
                    modification: Some(modification),
                })
            }
            (None, "note.preview") => Ok(Modified {
                result: json!({ "title": params["title"] }),
                modification: None,
            }),
            (None, _) => {
                let _held = self.title.lock().expect("title");
                Ok(Modified {
                    result: json!({}),
                    modification: Some(self.modifications.take()),
                })
            }
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
    ) -> impl Future<Output = Result<Modified<Value>, Failure>> + Send {
        self.commanded_by
            .lock()
            .expect("principals")
            .push(principal.clone());
        let outcome = match (&self.refusal, command) {
            (Some(refusal), _) => Err(Failure::refused(refusal.clone())),
            (None, "note.archive") => {
                let title = self.title.lock().expect("title");
                let modification = self.modifications.take();
                Ok(Modified {
                    result: json!({ "archived": *title }),
                    modification: Some(modification),
                })
            }
            (None, _) => Ok(Modified {
                result: json!({ "words": self.title().split_whitespace().count() }),
                modification: None,
            }),
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

    fn announcement_patch(
        &self,
        announcement: &Value,
        subscription: &ProjectionSubscription<Held<String>>,
    ) -> impl Future<Output = Option<Value>> + Send {
        self.announcement_patches_asked
            .lock()
            .expect("announcement patches")
            .push(subscription.projection.clone());
        // The authoring state reaches its subscribers another way.
        let patch = match subscription.projection.as_str() {
            "notes.list" => Some(json!({ "announced": announcement })),
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
fn only_a_rate_limit_a_base_revision_in_the_future_or_an_interruption_may_be_sent_again() {
    let retryable = ProjectionErrorCode::ALL
        .into_iter()
        .filter(|code| code.retryable())
        .collect::<Vec<_>>();

    assert_eq!(
        retryable,
        [
            ProjectionErrorCode::BaseRevisionInFuture,
            ProjectionErrorCode::RateLimit,
            ProjectionErrorCode::Interrupted
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
    let subscription_id = subscriptions.insert("notes.list".to_owned(), json!({}), 5, 0);

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
    let Some(Accepted::Mutation {
        mutation, result, ..
    }) = renamed.accepted
    else {
        panic!("an accepted mutation: {:?}", renamed.accepted);
    };
    assert_eq!(mutation, "note.rename");
    assert_eq!(result, json!({ "title": "Errands" }));
    let Some(Accepted::Command {
        command, result, ..
    }) = count.accepted
    else {
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

#[test]
fn a_command_runs_without_touching_a_subscription_and_is_answered_once_run() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let writer = Principal::new("writer", ActorKind::Human);

    let ran = block_on(run_command(
        &host,
        &writer,
        ProjectionCommandRequest {
            command: "note.archive".to_owned(),
            operation_id: Some("archive-op".to_owned()),
            params: json!({}),
        },
    ))
    .expect("archive");

    assert_eq!(*host.commanded_by.lock().expect("principals"), [writer]);
    assert!(host.command_patches_asked().is_empty());
    assert_eq!(
        host.subscription(notes.subscription_id).revision,
        notes.revision
    );

    let (revision, events) = block_on(publish_command(&host, &host.subscriptions, &ran));
    assert_eq!(
        patch_spans(&events),
        [(notes.subscription_id, notes.revision, revision)]
    );
    let reply = command_reply::<Notes>(&ran, revision).expect("a reply");
    let accepted: ProjectionCommandAccepted<Value> =
        serde_json::from_value(reply).expect("an accepted command");
    assert_eq!(accepted.operation_id.as_deref(), Some("archive-op"));
    assert_eq!(accepted.revision, revision);
    assert_eq!(
        accepted.result, ran.result,
        "the reply carries the host's result"
    );
}

#[test]
fn a_command_published_on_another_connection_patches_the_subscriptions_it_affects_there() {
    let host = Notes::new();
    let elsewhere = connection("other");
    let (notes, _) = subscribe(&host, "notes.list", None);
    let ran = block_on(run_command(
        &host,
        &Principal::new("other", ActorKind::Human),
        ProjectionCommandRequest {
            command: "note.archive".to_owned(),
            operation_id: None,
            params: json!({}),
        },
    ))
    .expect("archive");

    let (revision, events) = block_on(publish_command(&host, &host.subscriptions, &ran));
    let (_, elsewhere_events) = block_on(publish_command(&host, &elsewhere, &ran));

    assert_eq!(
        patch_spans(&events),
        [(notes.subscription_id, notes.revision, revision)]
    );
    assert!(elsewhere_events.is_empty(), "{elsewhere_events:?}");
}

#[test]
fn a_command_run_apart_from_its_connection_is_refused_addressed_to_it() {
    let host = Notes::new();

    let failure = block_on(run_command(
        &host,
        &Principal::new("writer", ActorKind::Human),
        ProjectionCommandRequest {
            command: "note.explode".to_owned(),
            operation_id: None,
            params: json!({}),
        },
    ))
    .expect_err("an unknown command");

    let envelope = envelope(&failure);
    assert_eq!(envelope.code, ProjectionErrorCode::UnknownCommand);
    assert_eq!(envelope.operation, ProjectionOperation::Command);
    assert_eq!(envelope.name.as_deref(), Some("note.explode"));
}

#[test]
fn a_decoded_request_is_refused_addressed_to_its_operation_when_its_params_do_not_decode() {
    assert!(decode_request::<Failure>("session.list", Some(json!({}))).is_none());

    let decoded = decode_request::<Failure>(
        PROJECTION_COMMAND_METHOD,
        Some(json!({ "command": "note.count", "params": {} })),
    )
    .expect("a protocol method")
    .expect("valid params");
    assert_eq!(decoded.name(), Some("note.count"));

    let failure = decode_request::<Failure>(PROJECTION_COMMAND_METHOD, None)
        .expect("a protocol method")
        .expect_err("missing params");
    let envelope = envelope(&failure);
    assert_eq!(envelope.code, ProjectionErrorCode::InvalidParams);
    assert_eq!(envelope.operation, ProjectionOperation::Command);
}

#[test]
fn an_announced_change_patches_every_subscription_reading_an_entity_it_touches_and_no_other() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let (_boards, _) = subscribe(&host, "boards.list", None);
    let announced = Announced {
        touches: &["Note"],
        modification: host.save("Errands"),
        announcement: json!({ "progress": 1 }),
    };

    let events = block_on(deliver_announcement(&host, &host.subscriptions, &announced));

    let [(subscription_id, from_revision, to_revision)] = patch_spans(&events)[..] else {
        panic!("one patch: {events:?}");
    };
    assert_eq!(subscription_id, notes.subscription_id);
    assert_eq!(from_revision, notes.revision);
    assert!(to_revision > from_revision);
    assert_eq!(host.announcement_patches_asked(), ["notes.list"]);
    assert_eq!(
        host.subscription(notes.subscription_id).revision,
        to_revision
    );
}

#[test]
fn an_announcement_touching_no_entity_a_projection_reads_patches_nothing() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);

    for touches in [&[][..], &["Clock"][..]] {
        let announced = Announced {
            touches,
            modification: host.save("Errands"),
            announcement: json!({}),
        };

        let events = block_on(deliver_announcement(&host, &host.subscriptions, &announced));

        assert!(events.is_empty(), "{events:?}");
    }
    assert!(host.announcement_patches_asked().is_empty());
    assert_eq!(
        host.subscription(notes.subscription_id).revision,
        notes.revision
    );
}

#[test]
fn successive_announcements_patch_a_subscription_in_the_order_they_were_made() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);

    let mut spans = Vec::new();
    for progress in 1..=3 {
        let announced = Announced {
            touches: &["Note"],
            modification: host.save(&format!("Errands {progress}")),
            announcement: json!({ "progress": progress }),
        };
        spans.extend(patch_spans(&block_on(deliver_announcement(
            &host,
            &host.subscriptions,
            &announced,
        ))));
    }

    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0].1, notes.revision);
    for pair in spans.windows(2) {
        assert_eq!(pair[1].1, pair[0].2, "{spans:?}");
    }
}

/// The command `command` run on `host`, as any connection's caller.
fn ran(host: &Notes, command: &str) -> RanCommand<Value> {
    block_on(run_command(
        host,
        &Principal::new("writer", ActorKind::Human),
        ProjectionCommandRequest {
            command: command.to_owned(),
            operation_id: None,
            params: json!({}),
        },
    ))
    .expect("the command runs")
}

#[test]
fn a_subscription_whose_snapshot_holds_a_command_s_modification_is_not_sent_it() {
    let host = Notes::new();
    let (before, _) = subscribe(&host, "notes.list", None);
    let archived = ran(&host, "note.archive");
    let (after, _) = subscribe(&host, "notes.list", None);

    let (_, events) = block_on(publish_command(&host, &host.subscriptions, &archived));

    let spans = patch_spans(&events);
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_eq!(spans[0].0, before.subscription_id);
    assert_eq!(host.command_patches_asked(), ["notes.list"]);
    assert_eq!(
        host.subscription(after.subscription_id).revision,
        after.revision
    );
}

#[test]
fn a_subscription_whose_snapshot_holds_a_mutation_made_elsewhere_is_not_sent_it() {
    let host = Notes::new();
    let elsewhere = connection("other");
    let (before, _) = subscribe(&host, "notes.list", None);
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
    let (after, _) = subscribe(&host, "notes.list", None);
    let accepted = reply
        .accepted
        .expect("the reply names the mutation it accepted");

    let (_, events) = block_on(publish(&host, &host.subscriptions, &accepted));

    let spans = patch_spans(&events);
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_eq!(spans[0].0, before.subscription_id);
    assert_eq!(
        host.subscription(after.subscription_id).revision,
        after.revision
    );
}

#[test]
fn a_subscription_whose_snapshot_holds_an_announced_modification_is_not_sent_it() {
    let host = Notes::new();
    let (before, _) = subscribe(&host, "notes.list", None);
    let announced = Announced {
        touches: &["Note"],
        modification: host.save("Errands"),
        announcement: json!({ "progress": 1 }),
    };
    let (after, _) = subscribe(&host, "notes.list", None);

    let events = block_on(deliver_announcement(&host, &host.subscriptions, &announced));

    let spans = patch_spans(&events);
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_eq!(spans[0].0, before.subscription_id);
    assert_eq!(
        host.subscription(after.subscription_id).revision,
        after.revision
    );
}

#[test]
fn each_modification_after_a_snapshot_is_sent_once_in_whatever_order_it_is_published() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let earlier = host.save("Errands");
    let later = host.save("Chores");

    let mut spans = Vec::new();
    for modification in [later, earlier] {
        let announced = Announced {
            touches: &["Note"],
            modification,
            announcement: json!({ "modification": modification }),
        };
        spans.extend(patch_spans(&block_on(deliver_announcement(
            &host,
            &host.subscriptions,
            &announced,
        ))));
    }

    assert_eq!(spans.len(), 2, "{spans:?}");
    assert!(spans.iter().all(|(id, _, _)| *id == notes.subscription_id));
}

#[test]
fn a_resent_snapshot_holds_every_modification_saved_before_it() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let modification = host.save("Errands");
    block_on(serve(
        &host,
        &host.subscriptions,
        ProjectionRequest::Resync(ProjectionResyncRequest {
            subscription_id: notes.subscription_id,
        }),
    ))
    .expect("resync");
    let announced = Announced {
        touches: &["Note"],
        modification,
        announcement: json!({ "progress": 1 }),
    };

    let events = block_on(deliver_announcement(&host, &host.subscriptions, &announced));

    assert!(events.is_empty(), "{events:?}");
    assert!(host.announcement_patches_asked().is_empty());
}

#[test]
fn a_change_that_saved_no_modification_sends_no_update() {
    let host = Notes::new();
    let (notes, _) = subscribe(&host, "notes.list", None);
    let unsaved = RanCommand {
        command: "note.archive".to_owned(),
        operation_id: None,
        result: json!({ "archived": host.title() }),
        modification: None,
    };

    let (revision, events) = block_on(publish_command(&host, &host.subscriptions, &unsaved));

    assert!(events.is_empty(), "{events:?}");
    assert_eq!(revision, notes.revision);
    assert!(host.command_patches_asked().is_empty());
}

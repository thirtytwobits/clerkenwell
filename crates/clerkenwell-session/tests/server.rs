//! One connection's projection commands served against an application host.

use std::future::Future;
use std::pin::pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};

use clerkenwell_schema::{
    GeneratedAuthoringPolicyKind, GeneratedEntityAuthoringSpec, GeneratedMaterializationPlan,
    GeneratedMutationSpec, GeneratedProjectionSpec,
};
use clerkenwell_session::transport::{
    ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionMutationAccepted,
    ProjectionMutationCommand, ProjectionOperation, ProjectionResyncAccepted,
    ProjectionResyncCommand, ProjectionSubscribeAccepted, ProjectionSubscribeCommand,
    ProjectionSubscribeCursor, ProjectionSubscribeResume, ProjectionTransportEvent,
    ProjectionUnsubscribeAccepted, ProjectionUnsubscribeCommand, PROJECTION_MUTATE_METHOD,
    PROJECTION_SUBSCRIBE_METHOD,
};
use clerkenwell_session::{
    serve, ProjectionCommand, ProjectionFailure, ProjectionHost, ProjectionRefusal,
    ProjectionRegistry, ProjectionReply, ProjectionSubscription, ProjectionSubscriptions,
};
use serde_json::{json, Value};

static PROJECTIONS: &[GeneratedProjectionSpec] = &[
    GeneratedProjectionSpec {
        name: "notes.list",
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
    GeneratedProjectionSpec {
        name: "notes.authoringState",
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
    rationale: "",
    mutations: &["note.rename", "note.preview"],
    content_mutation: None,
    planning_mutations: &["note.preview"],
    command_mutations: &["note.rename"],
    lifecycle_mutations: &[],
    session_mnemonic_key: None,
    conflict_policy: None,
}];
static REGISTRY: ProjectionRegistry = ProjectionRegistry::new(PROJECTIONS, MUTATIONS, AUTHORING);

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
    subscriptions: Mutex<ProjectionSubscriptions<Value, String>>,
    title: Mutex<String>,
    floor: u64,
    refusal: Option<ProjectionRefusal>,
    mutations_run: Mutex<usize>,
}

impl Notes {
    fn new() -> Self {
        Self {
            subscriptions: Mutex::new(ProjectionSubscriptions::default()),
            title: Mutex::new("Shopping".to_owned()),
            floor: 0,
            refusal: None,
            mutations_run: Mutex::new(0),
        }
    }

    fn title(&self) -> String {
        self.title.lock().expect("title").clone()
    }

    fn subscription(&self, subscription_id: u64) -> ProjectionSubscription<String> {
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
    type Failure = Failure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    async fn with_subscriptions<R: Send>(
        &self,
        transition: impl FnOnce(&mut ProjectionSubscriptions<Value, String>) -> R + Send,
    ) -> R {
        transition(&mut self.subscriptions.lock().expect("subscriptions"))
    }

    fn snapshot(
        &self,
        projection: &str,
        _params: &Value,
    ) -> impl Future<Output = Result<Value, Failure>> + Send {
        let snapshot = match projection {
            "notes.authoringState" => json!({ "frontier": format!("frontier:{}", self.title()) }),
            "boards.list" => json!({ "pinned": false }),
            _ => json!({ "title": self.title() }),
        };
        async move { Ok(snapshot) }
    }

    fn delivered(&self, snapshot: &Value) -> Option<String> {
        snapshot["frontier"].as_str().map(str::to_owned)
    }

    fn revision_floor(
        &self,
        _projection: &str,
        _params: &Value,
    ) -> impl Future<Output = Result<u64, Failure>> + Send {
        let floor = self.floor;
        async move { Ok(floor) }
    }

    fn mutation_revision_floor(&self) -> impl Future<Output = Result<u64, Failure>> + Send {
        let floor = self.floor;
        async move { Ok(floor) }
    }

    fn mutate(
        &self,
        mutation: &str,
        params: Value,
    ) -> impl Future<Output = Result<Value, Failure>> + Send {
        *self.mutations_run.lock().expect("count") += 1;
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
        subscription: &ProjectionSubscription<String>,
    ) -> impl Future<Output = Option<Value>> + Send {
        // The authoring state reaches its subscribers another way.
        let patch = match subscription.projection.as_str() {
            "notes.list" => Some(json!({ "title": result["title"] })),
            "boards.list" => Some(json!({ "pinned": true })),
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

type Reply = ProjectionReply<Value, Value>;

fn subscribe(
    host: &Notes,
    projection: &str,
    cursor: Option<u64>,
) -> (ProjectionSubscribeAccepted, Reply) {
    let reply = block_on(serve(
        host,
        ProjectionCommand::Subscribe(ProjectionSubscribeCommand {
            projection: projection.to_owned(),
            params: Some(json!({})),
            cursor: cursor.map(|revision| ProjectionSubscribeCursor { revision }),
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
        ProjectionCommand::Mutate(ProjectionMutationCommand {
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

fn rename(host: &Notes, title: &str) -> (ProjectionMutationAccepted<Value>, Reply) {
    mutate(host, "note.rename", json!({ "title": title })).expect("rename")
}

fn envelope(failure: &Failure) -> &ProjectionErrorEnvelope {
    failure
        .envelope
        .as_ref()
        .expect("a failure reaches its client described")
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
    assert_eq!(accepted.resume, None);
    assert_eq!(
        reply.events,
        [ProjectionTransportEvent::Snapshot {
            subscription_id: accepted.subscription_id,
            revision: accepted.revision,
            snapshot: json!({ "title": host.title() }),
        }]
    );
    assert_eq!(
        host.subscription(accepted.subscription_id).revision,
        accepted.revision
    );
}

#[test]
fn a_subscription_starts_no_lower_than_the_hosts_floor() {
    let host = Notes {
        floor: 40,
        ..Notes::new()
    };

    let (accepted, _) = subscribe(&host, "notes.list", None);

    assert!(accepted.revision >= host.floor);
}

#[test]
fn what_a_snapshot_leaves_its_client_holding_is_recorded() {
    let host = Notes::new();

    let (accepted, reply) = subscribe(&host, "notes.authoringState", None);

    let ProjectionTransportEvent::Snapshot { snapshot, .. } = &reply.events[0] else {
        panic!("a snapshot: {:?}", reply.events);
    };
    assert_eq!(
        host.subscription(accepted.subscription_id).delivered,
        host.delivered(snapshot)
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
    let (authoring, _) = subscribe(&host, "notes.authoringState", None);

    let (_, reply) = rename(&host, "Errands");

    assert!(reply.events.is_empty(), "{:?}", reply.events);
    assert_eq!(
        host.subscription(authoring.subscription_id).revision,
        authoring.revision
    );
}

#[test]
fn a_resuming_subscription_replays_the_patches_it_missed() {
    let host = Notes::new();
    let (first, _) = subscribe(&host, "notes.list", None);
    let (_, one) = rename(&host, "Errands");
    let (latest, two) = rename(&host, "Chores");

    let (resumed, reply) = subscribe(&host, "notes.list", Some(first.revision));

    assert_eq!(
        resumed.resume,
        Some(ProjectionSubscribeResume::Patches {
            from_revision: first.revision,
            revision: latest.revision,
            patch_count: 2,
        })
    );
    let sent = one
        .events
        .into_iter()
        .chain(two.events)
        .map(|event| match event {
            ProjectionTransportEvent::Patch { patch, .. } => patch,
            other => panic!("a patch: {other:?}"),
        });
    let replayed = reply.events.into_iter().map(|event| match event {
        ProjectionTransportEvent::Patch {
            subscription_id,
            patch,
            ..
        } => {
            assert_eq!(subscription_id, resumed.subscription_id);
            patch
        }
        other => panic!("a patch: {other:?}"),
    });
    assert!(sent.eq(replayed));
}

#[test]
fn a_subscription_already_at_the_current_revision_resumes_with_nothing_to_send() {
    let host = Notes::new();
    let (first, _) = subscribe(&host, "notes.list", None);

    let (resumed, reply) = subscribe(&host, "notes.list", Some(first.revision));

    assert_eq!(
        resumed.resume,
        Some(ProjectionSubscribeResume::UpToDate {
            revision: resumed.revision
        })
    );
    assert!(reply.events.is_empty());
}

#[test]
fn a_cursor_ahead_of_the_projection_is_refused() {
    let host = Notes::new();
    let (current, _) = subscribe(&host, "notes.list", None);

    let failure = block_on(serve(
        &host,
        ProjectionCommand::Subscribe(ProjectionSubscribeCommand {
            projection: "notes.list".to_owned(),
            params: None,
            cursor: Some(ProjectionSubscribeCursor {
                revision: current.revision + 10,
            }),
        }),
    ))
    .expect_err("a cursor ahead is refused");

    let envelope = envelope(&failure);
    assert_eq!(envelope.code, ProjectionErrorCode::CursorAhead);
    assert_eq!(envelope.operation, ProjectionOperation::Subscribe);
    assert_eq!(envelope.name.as_deref(), Some("notes.list"));
    assert!(!envelope.retryable);
}

#[test]
fn an_unknown_projection_is_refused_before_the_host_builds_anything() {
    let host = Notes::new();

    let failure = block_on(serve(
        &host,
        ProjectionCommand::Subscribe(ProjectionSubscribeCommand {
            projection: "notes.missing".to_owned(),
            params: None,
            cursor: None,
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
        ProjectionCommand::Resync(ProjectionResyncCommand {
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
        }]
    );
}

#[test]
fn resyncing_a_subscription_that_does_not_exist_is_refused_as_not_found() {
    let host = Notes::new();

    let failure = block_on(serve(
        &host,
        ProjectionCommand::Resync(ProjectionResyncCommand { subscription_id: 9 }),
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
            ProjectionCommand::Unsubscribe(ProjectionUnsubscribeCommand { subscription_id }),
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
fn a_mutation_reaches_no_lower_than_the_hosts_floor() {
    let host = Notes {
        floor: 90,
        ..Notes::new()
    };

    let (accepted, _) = rename(&host, "Errands");

    assert!(accepted.revision >= host.floor);
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
    let mut subscriptions = ProjectionSubscriptions::<Value, String>::default();
    let subscription_id = subscriptions.insert("notes.list".to_owned(), json!({}), 5);

    // A delivery reached the client after the mutation chose its revision.
    let event = subscriptions
        .deliver_patch::<Value>(subscription_id, 3, json!({ "title": "Errands" }))
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
fn only_the_protocols_methods_decode_as_commands() {
    assert!(ProjectionCommand::decode("session.list", Some(json!({}))).is_none());

    let decoded = ProjectionCommand::decode(
        PROJECTION_SUBSCRIBE_METHOD,
        Some(json!({ "projection": "notes.list" })),
    )
    .expect("a protocol method")
    .expect("valid params");
    assert_eq!(decoded.operation(), ProjectionOperation::Subscribe);
    assert_eq!(decoded.name(), Some("notes.list"));
}

#[test]
fn a_command_without_valid_params_is_refused_as_invalid_params() {
    for params in [None, Some(json!({ "mutation": 7 }))] {
        let refusal = ProjectionCommand::decode(PROJECTION_MUTATE_METHOD, params)
            .expect("a protocol method")
            .expect_err("invalid params");
        assert_eq!(refusal.code, ProjectionErrorCode::InvalidParams);
    }
}

//! Serving the projection session protocol on one connection.
//!
//! A transport hands each request to [`serve_request`] with the application's
//! [`ProjectionHost`] and the connection's subscriptions. It answers the
//! request with the reply's result and sends each of the reply's events as a
//! `projection.update` notification. A mutation the reply accepted, or a
//! command it served, reaches the subscriptions of every other connection
//! through [`publish`]. A change the application announces reaches every
//! connection's subscriptions through [`deliver_announcement`].
//!
//! A transport that serves a connection's commands while it serves the
//! connection's other requests decodes each request with [`decode_request`]
//! and runs each command with [`run_command`], which reads and changes no
//! subscription. Once a command has run, [`publish_command`] sends each
//! connection's subscriptions the patches it brings them, and
//! [`command_reply`] is its caller's answer.

use std::future::Future;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use clerkenwell_events::{ChangeEvent, ChangeKind, Principal};
use clerkenwell_schema::AuthoringStatePatch;

use crate::announcements::{Announced, AnnouncementFeed};
use crate::authoring::{AuthoringHeld, AuthoringStates, Held};
use crate::registry::ProjectionRegistry;
use crate::subscriptions::{ProjectionSubscription, ProjectionSubscriptions};
use crate::transport::{
    ProjectionCommandAccepted, ProjectionCommandRequest, ProjectionErrorCode,
    ProjectionErrorEnvelope, ProjectionMutationAccepted, ProjectionMutationRequest,
    ProjectionOperation, ProjectionResyncRequest, ProjectionSubscribeRequest,
    ProjectionTransportEvent, ProjectionUnsubscribeAccepted, ProjectionUnsubscribeRequest,
};

/// A projection protocol request.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionRequest {
    Subscribe(ProjectionSubscribeRequest),
    Resync(ProjectionResyncRequest),
    Unsubscribe(ProjectionUnsubscribeRequest),
    Mutate(ProjectionMutationRequest),
    Command(ProjectionCommandRequest),
}

impl ProjectionRequest {
    /// The request `method` names with `params`, or `None` when the method is
    /// not part of the protocol.
    pub fn decode(method: &str, params: Option<Value>) -> Option<Result<Self, ProjectionRefusal>> {
        Some(match ProjectionOperation::for_method(method)? {
            ProjectionOperation::Subscribe => decode_params(params).map(Self::Subscribe),
            ProjectionOperation::Resync => decode_params(params).map(Self::Resync),
            ProjectionOperation::Unsubscribe => decode_params(params).map(Self::Unsubscribe),
            ProjectionOperation::Mutate => decode_params(params).map(Self::Mutate),
            ProjectionOperation::Command => decode_params(params).map(Self::Command),
        })
    }

    pub fn operation(&self) -> ProjectionOperation {
        match self {
            Self::Subscribe(_) => ProjectionOperation::Subscribe,
            Self::Resync(_) => ProjectionOperation::Resync,
            Self::Unsubscribe(_) => ProjectionOperation::Unsubscribe,
            Self::Mutate(_) => ProjectionOperation::Mutate,
            Self::Command(_) => ProjectionOperation::Command,
        }
    }

    /// The projection, mutation or command the request names.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Subscribe(request) => Some(&request.projection),
            Self::Mutate(request) => Some(&request.mutation),
            Self::Command(request) => Some(&request.command),
            Self::Resync(_) | Self::Unsubscribe(_) => None,
        }
    }
}

fn decode_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, ProjectionRefusal> {
    let params = params.ok_or_else(|| {
        ProjectionRefusal::new(
            ProjectionErrorCode::InvalidParams,
            "This method requires params.",
            None,
        )
    })?;
    serde_json::from_value(params).map_err(|error| {
        ProjectionRefusal::new(ProjectionErrorCode::InvalidParams, error.to_string(), None)
    })
}

/// Why a request was refused, before the refusal is addressed to the request.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionRefusal {
    pub code: ProjectionErrorCode,
    pub message: String,
    pub details: Option<Value>,
}

impl ProjectionRefusal {
    pub fn new(
        code: ProjectionErrorCode,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }
}

/// A host's failure, which the session describes to the client whose request
/// it answers.
pub trait ProjectionFailure: Sized {
    /// The failure for a refusal the session raises itself.
    fn refused(refusal: ProjectionRefusal) -> Self;

    /// What the failure tells a client.
    fn describe(&self) -> ProjectionRefusal;

    /// This failure carrying `envelope` to its client.
    fn described(self, envelope: ProjectionErrorEnvelope) -> Self;
}

/// What an application provides to serve projections.
pub trait ProjectionHost: Sync {
    type Snapshot: Serialize + DeserializeOwned + Send;
    type Patch: Serialize + DeserializeOwned + Clone + PartialEq + Send;
    /// What an update leaves its client holding. A client sends it back when
    /// it subscribes again, and is sent only what it lacks.
    type Delivery: Clone + PartialEq + Serialize + DeserializeOwned + Send;
    type MutationResult: Serialize + Send + Sync;
    type CommandResult: Serialize + Send + Sync;
    /// The application's account of a change it makes outside any request.
    type Announcement: Send + Sync;
    type Failure: ProjectionFailure + Send;

    /// The projections, mutations and commands served.
    fn registry(&self) -> &ProjectionRegistry;

    /// The authoring states of the collaborative documents served, which the
    /// session answers and delivers itself; `None` when none are served.
    fn authoring_states(&self) -> Option<&AuthoringStates> {
        None
    }

    /// The feed the application announces the changes it makes outside any
    /// request on; `None` when it announces none.
    fn announcements(&self) -> Option<&AnnouncementFeed<Self::Announcement>> {
        None
    }

    /// `projection`'s state for a client that holds `held`, which may carry
    /// only what that client lacks. The session asks for no authoring state.
    fn snapshot(
        &self,
        projection: &str,
        params: &Value,
        held: Option<&Self::Delivery>,
    ) -> impl Future<Output = Result<Self::Snapshot, Self::Failure>> + Send;

    /// What `snapshot` leaves its client holding, for a projection that names
    /// it.
    fn delivered(&self, snapshot: &Self::Snapshot) -> Option<Self::Delivery>;

    /// What `patch` leaves its client holding, for a projection that names
    /// it.
    fn delivered_by_patch(&self, patch: &Self::Patch) -> Option<Self::Delivery>;

    /// Makes `mutation` as `principal`, the principal the host authenticated
    /// the request's connection as.
    fn mutate(
        &self,
        principal: &Principal,
        mutation: &str,
        params: Value,
    ) -> impl Future<Output = Result<Self::MutationResult, Self::Failure>> + Send;

    /// The patch an accepted mutation sends `subscription`, or `None` when its
    /// change reaches that subscription some other way. The session asks for
    /// no authoring state: those take each change their store announces.
    ///
    /// This and the other patch hooks may be asked for a subscription whose
    /// snapshot already holds the change: one accepted, or resent, while the
    /// change was being made. `subscription.delivered` is what its client
    /// holds, for the host to judge by.
    fn mutation_patch(
        &self,
        mutation: &str,
        result: &Self::MutationResult,
        subscription: &ProjectionSubscription<Held<Self::Delivery>>,
    ) -> impl Future<Output = Option<Self::Patch>> + Send;

    /// Runs `command` as `principal`, the principal the host authenticated
    /// the request's connection as.
    fn command(
        &self,
        principal: &Principal,
        command: &str,
        params: Value,
    ) -> impl Future<Output = Result<Self::CommandResult, Self::Failure>> + Send;

    /// The patch a served command sends `subscription`, or `None` when its
    /// change reaches that subscription some other way. The session asks only
    /// for subscriptions to a projection that reads an entity the command
    /// touches, and for no authoring state.
    fn command_patch(
        &self,
        command: &str,
        result: &Self::CommandResult,
        subscription: &ProjectionSubscription<Held<Self::Delivery>>,
    ) -> impl Future<Output = Option<Self::Patch>> + Send;

    /// The patch an announced change sends `subscription`, or `None` when its
    /// change reaches that subscription some other way. The session asks only
    /// for subscriptions to a projection that reads an entity the change
    /// touches, and for no authoring state.
    fn announcement_patch(
        &self,
        announcement: &Self::Announcement,
        subscription: &ProjectionSubscription<Held<Self::Delivery>>,
    ) -> impl Future<Output = Option<Self::Patch>> + Send;
}

/// A connection's subscriptions, which the session changes one transition at
/// a time.
pub trait ProjectionConnection<P, D>: Sync {
    /// Runs `transition` on the subscriptions, holding them for the
    /// transition only.
    fn with_subscriptions<R: Send>(
        &self,
        transition: impl FnOnce(&mut ProjectionSubscriptions<P, D>) -> R + Send,
    ) -> impl Future<Output = R> + Send;
}

impl<P: Send, D: Send> ProjectionConnection<P, D> for Mutex<ProjectionSubscriptions<P, D>> {
    async fn with_subscriptions<R: Send>(
        &self,
        transition: impl FnOnce(&mut ProjectionSubscriptions<P, D>) -> R + Send,
    ) -> R {
        transition(&mut self.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// A mutation one connection accepted, or a command it served, as every
/// connection publishes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Accepted<M, C> {
    Mutation { mutation: String, result: M },
    Command { command: String, result: C },
}

/// A served request's result and the updates it sends.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionReply<S, P, M, C> {
    /// The accepted request, as the protocol encodes it.
    pub result: Value,
    /// The updates to send the connection's subscriptions, in order.
    pub events: Vec<ProjectionTransportEvent<S, P>>,
    /// The mutation the request made, or the command it ran, for the other
    /// connections to publish.
    pub accepted: Option<Accepted<M, C>>,
}

type Reply<H> = ProjectionReply<
    <H as ProjectionHost>::Snapshot,
    <H as ProjectionHost>::Patch,
    <H as ProjectionHost>::MutationResult,
    <H as ProjectionHost>::CommandResult,
>;
type Change<H> =
    Accepted<<H as ProjectionHost>::MutationResult, <H as ProjectionHost>::CommandResult>;

/// What a delivery brings a connection's subscriptions: a mutation's change, a
/// command's, or one the application announced.
enum Source<'a, H: ProjectionHost> {
    Mutation {
        mutation: &'a str,
        result: &'a H::MutationResult,
    },
    Command {
        command: &'a str,
        result: &'a H::CommandResult,
    },
    Announced(&'a Announced<H::Announcement>),
}

impl<'a, H: ProjectionHost> Source<'a, H> {
    fn accepted(accepted: &'a Change<H>) -> Self {
        match accepted {
            Accepted::Mutation { mutation, result } => Self::Mutation { mutation, result },
            Accepted::Command { command, result } => Self::Command { command, result },
        }
    }
}
type Event<H> =
    ProjectionTransportEvent<<H as ProjectionHost>::Snapshot, <H as ProjectionHost>::Patch>;

/// Serves the request `method` names with `params` on `connection`, or
/// returns `None` when the method is not part of the protocol. A request
/// whose params do not decode is refused like a request that fails.
pub async fn serve_request<H, C>(
    host: &H,
    connection: &C,
    method: &str,
    params: Option<Value>,
) -> Option<Result<Reply<H>, H::Failure>>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    Some(match decode_request(method, params)? {
        Ok(request) => serve(host, connection, request).await,
        Err(failure) => Err(failure),
    })
}

/// The request `method` names with `params`, or `None` when the method is
/// not part of the protocol. A request whose params do not decode is refused,
/// addressed to its operation.
pub fn decode_request<F: ProjectionFailure>(
    method: &str,
    params: Option<Value>,
) -> Option<Result<ProjectionRequest, F>> {
    let operation = ProjectionOperation::for_method(method)?;
    Some(
        ProjectionRequest::decode(method, params)?
            .map_err(|refusal| addressed(F::refused(refusal), operation, None)),
    )
}

/// A command its host ran, for its connection to answer.
#[derive(Debug, Clone, PartialEq)]
pub struct RanCommand<C> {
    pub command: String,
    pub operation_id: Option<String>,
    pub result: C,
}

/// Runs the command `request` names as `principal`, the principal the host
/// authenticated the request's connection as. It reads and changes no
/// subscription. A failure reaches its client described by a
/// [`ProjectionErrorEnvelope`] addressed to the command.
pub async fn run_command<H: ProjectionHost>(
    host: &H,
    principal: &Principal,
    request: ProjectionCommandRequest,
) -> Result<RanCommand<H::CommandResult>, H::Failure> {
    let name = request.command.clone();
    execute_command(host, principal, request)
        .await
        .map_err(|failure| addressed(failure, ProjectionOperation::Command, Some(&name)))
}

/// Sends the subscriptions on `connection` the patches the command `ran`
/// brings them. Returns the revision its effects reach there and the updates
/// to send, in order.
pub async fn publish_command<H, C>(
    host: &H,
    connection: &C,
    ran: &RanCommand<H::CommandResult>,
) -> (u64, Vec<Event<H>>)
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let source = Source::Command {
        command: &ran.command,
        result: &ran.result,
    };
    deliver(host, connection, source).await
}

/// The answer to the command `ran`, whose effects reach `revision` on its
/// caller's connection, as the protocol encodes it. A failure reaches its
/// client described by a [`ProjectionErrorEnvelope`] addressed to the command.
pub fn command_reply<H: ProjectionHost>(
    ran: &RanCommand<H::CommandResult>,
    revision: u64,
) -> Result<Value, H::Failure> {
    encode::<H, _>(&ProjectionCommandAccepted {
        operation_id: ran.operation_id.clone(),
        revision,
        result: &ran.result,
    })
    .map_err(|failure| addressed(failure, ProjectionOperation::Command, Some(&ran.command)))
}

/// Serves one request on `connection`. A failure reaches its client described
/// by a [`ProjectionErrorEnvelope`] addressed to the request.
pub async fn serve<H, C>(
    host: &H,
    connection: &C,
    request: ProjectionRequest,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let operation = request.operation();
    let name = request.name().map(str::to_owned);
    let started = Instant::now();
    let outcome = match request {
        ProjectionRequest::Subscribe(request) => subscribe(host, connection, request).await,
        ProjectionRequest::Resync(request) => resync(host, connection, request).await,
        ProjectionRequest::Unsubscribe(request) => unsubscribe::<H, C>(connection, request).await,
        ProjectionRequest::Mutate(request) => mutate(host, connection, request).await,
        ProjectionRequest::Command(request) => {
            let principal = principal_of(connection).await;
            match execute_command(host, &principal, request).await {
                Ok(ran) => reply_to_command(host, connection, ran).await,
                Err(failure) => Err(failure),
            }
        }
    };
    let outcome = outcome.map_err(|failure| addressed(failure, operation, name.as_deref()));
    if operation == ProjectionOperation::Mutate {
        let latency = started.elapsed();
        let rejection = outcome
            .as_ref()
            .err()
            .map(|failure| failure.describe().code.as_str());
        connection
            .with_subscriptions(move |subscriptions| {
                subscriptions.record_mutation(latency, rejection);
            })
            .await;
    }
    outcome
}

/// Sends the subscriptions on `connection` the patches an accepted mutation,
/// or a served command, brings them. Returns the revision its effects reach
/// there and the updates to send, in order.
pub async fn publish<H, C>(host: &H, connection: &C, accepted: &Change<H>) -> (u64, Vec<Event<H>>)
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    deliver(host, connection, Source::accepted(accepted)).await
}

/// Sends the subscriptions on `connection` the patches a change the
/// application announced brings them, in order.
pub async fn deliver_announcement<H, C>(
    host: &H,
    connection: &C,
    announced: &Announced<H::Announcement>,
) -> Vec<Event<H>>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    deliver(host, connection, Source::Announced(announced))
        .await
        .1
}

/// Sends the subscriptions on `connection` the patches `source` brings them.
/// Returns the revision its effects reach there and the updates to send, in
/// order.
async fn deliver<H, C>(host: &H, connection: &C, source: Source<'_, H>) -> (u64, Vec<Event<H>>)
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let registry = host.registry();
    let publishes = match &source {
        Source::Mutation { mutation, .. } => !registry.plans_only(mutation),
        Source::Command { command, .. } => registry
            .command(command)
            .is_some_and(|spec| !spec.touches.is_empty()),
        Source::Announced(announced) => !announced.touches.is_empty(),
    };
    let affects = |projection: &str| match &source {
        Source::Mutation { mutation, .. } => {
            registry.mutation_affects_projection(mutation, projection)
        }
        Source::Command { command, .. } => registry.command_affects_projection(command, projection),
        Source::Announced(announced) => {
            registry.entities_affect_projection(announced.touches, projection)
        }
    };
    let revision = connection
        .with_subscriptions(move |subscriptions| subscriptions.change_revision(publishes))
        .await;
    let mut events = Vec::new();
    if !publishes {
        return (revision, events);
    }
    let affected = connection
        .with_subscriptions(|subscriptions| {
            let mut affected = subscriptions
                .all()
                .iter()
                .filter(|(_, subscription)| {
                    affects(&subscription.projection)
                        && !is_authoring(host, &subscription.projection)
                })
                .map(|(subscription_id, subscription)| (*subscription_id, subscription.clone()))
                .collect::<Vec<_>>();
            affected.sort_by_key(|(subscription_id, _)| *subscription_id);
            affected
        })
        .await;
    for (subscription_id, subscription) in affected {
        let patch = match &source {
            Source::Mutation { mutation, result } => {
                host.mutation_patch(mutation, result, &subscription).await
            }
            Source::Command { command, result } => {
                host.command_patch(command, result, &subscription).await
            }
            Source::Announced(announced) => {
                host.announcement_patch(&announced.announcement, &subscription)
                    .await
            }
        };
        let Some(patch) = patch else {
            continue;
        };
        let delivered = host.delivered_by_patch(&patch).map(Held::Host);
        let event = connection
            .with_subscriptions(move |subscriptions| {
                subscriptions.deliver_patch(subscription_id, revision, patch, delivered)
            })
            .await;
        events.extend(event);
    }
    (revision, events)
}

/// Sends every subscription on `connection` a snapshot, as after the
/// connection missed updates, recording `reason` as why. A subscription whose
/// snapshot cannot be built keeps the revision it holds.
pub async fn resync_all<H, C>(host: &H, connection: &C, reason: &str) -> Vec<Event<H>>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let mut subscriptions = connection
        .with_subscriptions(|subscriptions| {
            subscriptions
                .all()
                .iter()
                .map(|(subscription_id, subscription)| (*subscription_id, subscription.clone()))
                .collect::<Vec<_>>()
        })
        .await;
    subscriptions.sort_by_key(|(subscription_id, _)| *subscription_id);
    let principal = principal_of(connection).await;
    let mut events = Vec::new();
    for (subscription_id, subscription) in subscriptions {
        let Ok((snapshot, delivered)) = snapshot_for(
            host,
            &principal,
            &subscription.projection,
            &subscription.params,
            None,
        )
        .await
        else {
            continue;
        };
        let reason = reason.to_owned();
        let resynced = connection
            .with_subscriptions(move |subscriptions| {
                subscriptions.resync(subscription_id, snapshot, delivered, &reason)
            })
            .await;
        events.extend(resynced.map(|(_, event)| event));
    }
    events
}

/// Sends the subscriptions on `connection` that follow the document `change`
/// names what the change brings them: the accepted operations a commit adds,
/// or its removal where it was deleted. A subscription whose client already
/// holds the change is sent nothing.
pub async fn deliver_change<H, C>(host: &H, connection: &C, change: &ChangeEvent) -> Vec<Event<H>>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let Some(authoring) = host.authoring_states() else {
        return Vec::new();
    };
    let mut subscriptions = connection
        .with_subscriptions(|subscriptions| {
            subscriptions
                .all()
                .iter()
                .filter(|(_, subscription)| {
                    authoring
                        .plan(&subscription.projection)
                        .is_some_and(|plan| plan.name == change.data.entity)
                })
                .map(|(subscription_id, subscription)| (*subscription_id, subscription.clone()))
                .collect::<Vec<_>>()
        })
        .await;
    subscriptions.sort_by_key(|(subscription_id, _)| *subscription_id);
    let principal = principal_of(connection).await;
    let mut events = Vec::new();
    for (subscription_id, subscription) in subscriptions {
        let Some(Ok(document)) = authoring.follow(&subscription.projection, &subscription.params)
        else {
            continue;
        };
        if document.store != change.source {
            continue;
        }
        if document.id.resource_id != change.data.resource_id {
            continue;
        }
        if change.kind == ChangeKind::Deleted {
            if subscription.delivered.is_none() {
                continue;
            }
            let Ok(patch) =
                authoring_patch::<H>(&subscription.projection, AuthoringStatePatch::remove())
            else {
                continue;
            };
            events.extend(
                connection
                    .with_subscriptions(move |subscriptions| {
                        subscriptions.deliver_patch(subscription_id, 0, patch, None)
                    })
                    .await,
            );
            continue;
        }
        let held = subscription.delivered.as_ref().and_then(Held::authoring);
        let Ok(state) = authoring.state(&document, held, &principal) else {
            continue;
        };
        let delivered = Held::Authoring(AuthoringHeld::from(&state));
        let Ok(patch) = authoring_patch::<H>(
            &subscription.projection,
            AuthoringStatePatch::replace(state),
        ) else {
            continue;
        };
        let from_revision = subscription.revision;
        let delivery = connection
            .with_subscriptions(move |subscriptions| {
                subscriptions.deliver_following(subscription_id, from_revision, patch, delivered)
            })
            .await;
        events.extend(delivery.ok());
    }
    events
}

fn is_authoring<H: ProjectionHost>(host: &H, projection: &str) -> bool {
    host.authoring_states()
        .is_some_and(|authoring| authoring.plan(projection).is_some())
}

/// The principal the host authenticated `connection` as.
async fn principal_of<P: Send, D: Send>(connection: &impl ProjectionConnection<P, D>) -> Principal {
    connection
        .with_subscriptions(|subscriptions| subscriptions.principal().clone())
        .await
}

/// A subscription's snapshot for a client that holds `held`, served to
/// `principal`, and what it leaves the client holding.
async fn snapshot_for<H: ProjectionHost>(
    host: &H,
    principal: &Principal,
    projection: &str,
    params: &Value,
    held: Option<&Held<H::Delivery>>,
) -> Result<(H::Snapshot, Option<Held<H::Delivery>>), H::Failure> {
    let authoring = host.authoring_states();
    if let Some(document) = authoring.and_then(|authoring| authoring.follow(projection, params)) {
        let document = document.map_err(H::Failure::refused)?;
        let state = authoring
            .expect("a followed document is an authoring state's")
            .state(&document, held.and_then(Held::authoring), principal)
            .map_err(H::Failure::refused)?;
        let delivered = Held::Authoring(AuthoringHeld::from(&state));
        return Ok((
            typed::<H, _>(json!({ "projection": projection, "value": state }))?,
            Some(delivered),
        ));
    }
    let held = match held {
        Some(Held::Host(held)) => Some(held),
        _ => None,
    };
    let snapshot = host.snapshot(projection, params, held).await?;
    let delivered = host.delivered(&snapshot).map(Held::Host);
    Ok((snapshot, delivered))
}

/// `patch` to `projection`, as the host's type for it.
fn authoring_patch<H: ProjectionHost>(
    projection: &str,
    patch: AuthoringStatePatch,
) -> Result<H::Patch, H::Failure> {
    typed::<H, _>(json!({ "projection": projection, "value": patch }))
}

/// `value` as the host's type for it.
fn typed<H: ProjectionHost, T: DeserializeOwned>(value: Value) -> Result<T, H::Failure> {
    serde_json::from_value(value).map_err(|error| {
        H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::InternalError,
            format!("The host's model does not hold the authoring state: {error}"),
            None,
        ))
    })
}

/// `failure` described to the client of the request it answers.
fn addressed<F: ProjectionFailure>(
    failure: F,
    operation: ProjectionOperation,
    name: Option<&str>,
) -> F {
    let refusal = failure.describe();
    failure.described(ProjectionErrorEnvelope::new(
        refusal.code,
        refusal.message,
        operation,
        name,
        refusal.details,
    ))
}

async fn subscribe<H, C>(
    host: &H,
    connection: &C,
    request: ProjectionSubscribeRequest,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    if host.registry().projection(&request.projection).is_none() {
        return Err(H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::UnknownProjection,
            format!("Unknown projection \"{}\".", request.projection),
            Some(json!({ "projection": request.projection })),
        )));
    }
    let params = request.params.unwrap_or_else(|| json!({}));
    let held = request.held.and_then(|held| {
        if is_authoring(host, &request.projection) {
            serde_json::from_value(held).ok().map(Held::Authoring)
        } else {
            serde_json::from_value(held).ok().map(Held::Host)
        }
    });
    let principal = principal_of(connection).await;
    let (snapshot, delivered) = snapshot_for(
        host,
        &principal,
        &request.projection,
        &params,
        held.as_ref(),
    )
    .await?;
    let projection = request.projection;
    let (accepted, events) = connection
        .with_subscriptions(move |subscriptions| {
            subscriptions.accept_subscription(projection, params, held, snapshot, delivered)
        })
        .await;
    Ok(ProjectionReply {
        result: encode::<H, _>(&accepted)?,
        events,
        accepted: None,
    })
}

async fn resync<H, C>(
    host: &H,
    connection: &C,
    request: ProjectionResyncRequest,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let subscription_id = request.subscription_id;
    let subscription = connection
        .with_subscriptions(move |subscriptions| subscriptions.get(subscription_id).cloned())
        .await
        .ok_or_else(|| subscription_not_found::<H>(subscription_id))?;
    let principal = principal_of(connection).await;
    let (snapshot, delivered) = snapshot_for(
        host,
        &principal,
        &subscription.projection,
        &subscription.params,
        None,
    )
    .await?;
    let (accepted, event) = connection
        .with_subscriptions(move |subscriptions| {
            subscriptions.resync(subscription_id, snapshot, delivered, "clientRequested")
        })
        .await
        .ok_or_else(|| subscription_not_found::<H>(subscription_id))?;
    Ok(ProjectionReply {
        result: encode::<H, _>(&accepted)?,
        events: vec![event],
        accepted: None,
    })
}

fn subscription_not_found<H: ProjectionHost>(subscription_id: u64) -> H::Failure {
    H::Failure::refused(ProjectionRefusal::new(
        ProjectionErrorCode::NotFound,
        format!("Projection subscription {subscription_id} does not exist."),
        Some(json!({ "subscription_id": subscription_id })),
    ))
}

async fn unsubscribe<H, C>(
    connection: &C,
    request: ProjectionUnsubscribeRequest,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let removed = connection
        .with_subscriptions(move |subscriptions| subscriptions.remove(request.subscription_id))
        .await;
    Ok(ProjectionReply {
        result: encode::<H, _>(&ProjectionUnsubscribeAccepted { removed })?,
        events: Vec::new(),
        accepted: None,
    })
}

async fn mutate<H, C>(
    host: &H,
    connection: &C,
    request: ProjectionMutationRequest,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let mutation = request.mutation;
    if host.registry().mutation(&mutation).is_none() {
        return Err(H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::UnknownMutation,
            format!("Unknown mutation \"{mutation}\"."),
            Some(json!({ "mutation": mutation })),
        )));
    }
    let principal = principal_of(connection).await;
    let result = host.mutate(&principal, &mutation, request.params).await?;
    let encoded = encode::<H, _>(&result)?;
    let accepted = Accepted::Mutation { mutation, result };
    let (revision, events) = publish(host, connection, &accepted).await;
    let reply = ProjectionMutationAccepted {
        operation_id: request.operation_id,
        base_revision: request.base_revision,
        revision,
        result: encoded,
    };
    Ok(ProjectionReply {
        result: encode::<H, _>(&reply)?,
        events,
        accepted: Some(accepted),
    })
}

async fn execute_command<H: ProjectionHost>(
    host: &H,
    principal: &Principal,
    request: ProjectionCommandRequest,
) -> Result<RanCommand<H::CommandResult>, H::Failure> {
    let command = request.command;
    if host.registry().command(&command).is_none() {
        return Err(H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::UnknownCommand,
            format!("Unknown command \"{command}\"."),
            Some(json!({ "command": command })),
        )));
    }
    let result = host.command(principal, &command, request.params).await?;
    Ok(RanCommand {
        command,
        operation_id: request.operation_id,
        result,
    })
}

async fn reply_to_command<H, C>(
    host: &H,
    connection: &C,
    ran: RanCommand<H::CommandResult>,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let (revision, events) = publish_command(host, connection, &ran).await;
    let result = encode::<H, _>(&ProjectionCommandAccepted {
        operation_id: ran.operation_id.clone(),
        revision,
        result: &ran.result,
    })?;
    Ok(ProjectionReply {
        result,
        events,
        accepted: Some(Accepted::Command {
            command: ran.command,
            result: ran.result,
        }),
    })
}

fn encode<H: ProjectionHost, T: Serialize>(value: &T) -> Result<Value, H::Failure> {
    serde_json::to_value(value).map_err(|error| {
        H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::InternalError,
            format!("Could not encode the projection result: {error}"),
            None,
        ))
    })
}

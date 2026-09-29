//! Serving the projection session protocol on one connection.
//!
//! A transport hands each request to [`serve_request`] with the application's
//! [`ProjectionHost`] and the connection's subscriptions. It answers the
//! request with the reply's result and sends each of the reply's events as a
//! `projection.update` notification. A mutation the reply accepted reaches the
//! subscriptions of every other connection through [`publish`].

use std::future::Future;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use clerkenwell_events::{ChangeEvent, ChangeKind};

use crate::authoring::{AuthoringHeld, AuthoringState, AuthoringStates, Held};
use crate::registry::ProjectionRegistry;
use crate::subscriptions::{ProjectionSubscription, ProjectionSubscriptions};
use crate::transport::{
    ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionMutationAccepted,
    ProjectionMutationCommand, ProjectionOperation, ProjectionResyncCommand,
    ProjectionSubscribeCommand, ProjectionTransportEvent, ProjectionUnsubscribeAccepted,
    ProjectionUnsubscribeCommand,
};

/// A projection protocol request.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionCommand {
    Subscribe(ProjectionSubscribeCommand),
    Resync(ProjectionResyncCommand),
    Unsubscribe(ProjectionUnsubscribeCommand),
    Mutate(ProjectionMutationCommand),
}

impl ProjectionCommand {
    /// The command `method` names with `params`, or `None` when the method is
    /// not part of the protocol.
    pub fn decode(method: &str, params: Option<Value>) -> Option<Result<Self, ProjectionRefusal>> {
        Some(match ProjectionOperation::for_method(method)? {
            ProjectionOperation::Subscribe => decode_params(params).map(Self::Subscribe),
            ProjectionOperation::Resync => decode_params(params).map(Self::Resync),
            ProjectionOperation::Unsubscribe => decode_params(params).map(Self::Unsubscribe),
            ProjectionOperation::Mutate => decode_params(params).map(Self::Mutate),
        })
    }

    pub fn operation(&self) -> ProjectionOperation {
        match self {
            Self::Subscribe(_) => ProjectionOperation::Subscribe,
            Self::Resync(_) => ProjectionOperation::Resync,
            Self::Unsubscribe(_) => ProjectionOperation::Unsubscribe,
            Self::Mutate(_) => ProjectionOperation::Mutate,
        }
    }

    /// The projection or mutation the command names.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Subscribe(command) => Some(&command.projection),
            Self::Mutate(command) => Some(&command.mutation),
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

/// Why a command was refused, before the refusal is addressed to the command.
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

/// A host's failure, which the session describes to the client whose command
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
    type Failure: ProjectionFailure + Send;

    /// The projections and mutations served.
    fn registry(&self) -> &ProjectionRegistry;

    /// The authoring states of the collaborative documents served, which the
    /// session answers and delivers itself; `None` when none are served.
    fn authoring_states(&self) -> Option<&AuthoringStates> {
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

    fn mutate(
        &self,
        mutation: &str,
        params: Value,
    ) -> impl Future<Output = Result<Self::MutationResult, Self::Failure>> + Send;

    /// The patch an accepted mutation sends `subscription`, or `None` when its
    /// change reaches that subscription some other way. The session asks for
    /// no authoring state: those take each change their store announces.
    fn mutation_patch(
        &self,
        mutation: &str,
        result: &Self::MutationResult,
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

/// A mutation one connection accepted, as every connection publishes it.
#[derive(Debug, Clone, PartialEq)]
pub struct AcceptedMutation<M> {
    pub mutation: String,
    pub result: M,
}

/// A served command's result and the updates it sends.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionReply<S, P, M> {
    /// The accepted command, as the protocol encodes it.
    pub result: Value,
    /// The updates to send the connection's subscriptions, in order.
    pub events: Vec<ProjectionTransportEvent<S, P>>,
    /// The mutation the command made, for the other connections to publish.
    pub accepted: Option<AcceptedMutation<M>>,
}

type Reply<H> = ProjectionReply<
    <H as ProjectionHost>::Snapshot,
    <H as ProjectionHost>::Patch,
    <H as ProjectionHost>::MutationResult,
>;
type Event<H> =
    ProjectionTransportEvent<<H as ProjectionHost>::Snapshot, <H as ProjectionHost>::Patch>;

/// Serves the request `method` names with `params` on `connection`, or
/// returns `None` when the method is not part of the protocol. A request
/// whose params do not decode is refused like a command that fails.
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
    let operation = ProjectionOperation::for_method(method)?;
    Some(match ProjectionCommand::decode(method, params)? {
        Ok(command) => serve(host, connection, command).await,
        Err(refusal) => Err(addressed(H::Failure::refused(refusal), operation, None)),
    })
}

/// Serves one command on `connection`. A failure reaches its client described
/// by a [`ProjectionErrorEnvelope`] addressed to the command.
pub async fn serve<H, C>(
    host: &H,
    connection: &C,
    command: ProjectionCommand,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let operation = command.operation();
    let name = command.name().map(str::to_owned);
    let started = Instant::now();
    let outcome = match command {
        ProjectionCommand::Subscribe(command) => subscribe(host, connection, command).await,
        ProjectionCommand::Resync(command) => resync(host, connection, command).await,
        ProjectionCommand::Unsubscribe(command) => unsubscribe::<H, C>(connection, command).await,
        ProjectionCommand::Mutate(command) => mutate(host, connection, command).await,
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

/// Sends the subscriptions on `connection` the patches an accepted mutation
/// brings them. Returns the revision the mutation's effects reach there and
/// the updates to send, in order.
pub async fn publish<H, C>(
    host: &H,
    connection: &C,
    accepted: &AcceptedMutation<H::MutationResult>,
) -> (u64, Vec<Event<H>>)
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let registry = host.registry();
    let mutation = accepted.mutation.as_str();
    let publishes = !registry.plans_only(mutation);
    let revision = connection
        .with_subscriptions(move |subscriptions| subscriptions.mutation_revision(publishes))
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
                    registry.mutation_affects_projection(mutation, &subscription.projection)
                        && !is_authoring(host, &subscription.projection)
                })
                .map(|(subscription_id, subscription)| (*subscription_id, subscription.clone()))
                .collect::<Vec<_>>();
            affected.sort_by_key(|(subscription_id, _)| *subscription_id);
            affected
        })
        .await;
    for (subscription_id, subscription) in affected {
        let Some(patch) = host
            .mutation_patch(mutation, &accepted.result, &subscription)
            .await
        else {
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
    let mut events = Vec::new();
    for (subscription_id, subscription) in subscriptions {
        let Ok((snapshot, delivered)) =
            snapshot_for(host, &subscription.projection, &subscription.params, None).await
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
/// the document where it moved to, or its removal where it was deleted or
/// moved from. A subscription whose client already holds the change is sent
/// nothing.
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
    let mut events = Vec::new();
    for (subscription_id, subscription) in subscriptions {
        let Some(Ok(document)) = authoring.follow(&subscription.projection, &subscription.params)
        else {
            continue;
        };
        if document.store != change.source {
            continue;
        }
        let named_now = document.id.resource_id == change.data.resource_id;
        let named_before = change.kind == ChangeKind::Moved
            && change.data.moved_from.as_deref() == Some(document.id.resource_id.as_str());
        if named_before || (named_now && change.kind == ChangeKind::Deleted) {
            if subscription.delivered.is_none() {
                continue;
            }
            let Ok(patch) = authoring_patch::<H>(&subscription.projection, None) else {
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
        if !named_now {
            continue;
        }
        // A document moved here shares no history with one its client held.
        let held = match change.kind {
            ChangeKind::Moved => None,
            _ => subscription.delivered.as_ref().and_then(Held::authoring),
        };
        let Ok(state) = authoring.state(&document, held) else {
            continue;
        };
        let Ok(patch) = authoring_patch::<H>(&subscription.projection, Some(&state)) else {
            continue;
        };
        let delivered = Held::Authoring(AuthoringHeld::from(&state));
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

/// A subscription's snapshot for a client that holds `held`, and what it
/// leaves the client holding.
async fn snapshot_for<H: ProjectionHost>(
    host: &H,
    projection: &str,
    params: &Value,
    held: Option<&Held<H::Delivery>>,
) -> Result<(H::Snapshot, Option<Held<H::Delivery>>), H::Failure> {
    let authoring = host.authoring_states();
    if let Some(document) = authoring.and_then(|authoring| authoring.follow(projection, params)) {
        let document = document.map_err(H::Failure::refused)?;
        let state = authoring
            .expect("a followed document is an authoring state's")
            .state(&document, held.and_then(Held::authoring))
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

/// The authoring-state patch delivering `state`, or removing the document.
fn authoring_patch<H: ProjectionHost>(
    projection: &str,
    state: Option<&AuthoringState>,
) -> Result<H::Patch, H::Failure> {
    let value = match state {
        Some(state) => json!({ "kind": "replace", "state": state }),
        None => json!({ "kind": "remove" }),
    };
    typed::<H, _>(json!({ "projection": projection, "value": value }))
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

/// `failure` described to the client of the command it answers.
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
    command: ProjectionSubscribeCommand,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    if host.registry().projection(&command.projection).is_none() {
        return Err(H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::UnknownProjection,
            format!("Unknown projection \"{}\".", command.projection),
            Some(json!({ "projection": command.projection })),
        )));
    }
    let params = command.params.unwrap_or_else(|| json!({}));
    let held = command.held.and_then(|held| {
        if is_authoring(host, &command.projection) {
            serde_json::from_value(held).ok().map(Held::Authoring)
        } else {
            serde_json::from_value(held).ok().map(Held::Host)
        }
    });
    let (snapshot, delivered) =
        snapshot_for(host, &command.projection, &params, held.as_ref()).await?;
    let projection = command.projection;
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
    command: ProjectionResyncCommand,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let subscription_id = command.subscription_id;
    let subscription = connection
        .with_subscriptions(move |subscriptions| subscriptions.get(subscription_id).cloned())
        .await
        .ok_or_else(|| subscription_not_found::<H>(subscription_id))?;
    let (snapshot, delivered) =
        snapshot_for(host, &subscription.projection, &subscription.params, None).await?;
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
    command: ProjectionUnsubscribeCommand,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let removed = connection
        .with_subscriptions(move |subscriptions| subscriptions.remove(command.subscription_id))
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
    command: ProjectionMutationCommand,
) -> Result<Reply<H>, H::Failure>
where
    H: ProjectionHost,
    C: ProjectionConnection<H::Patch, Held<H::Delivery>>,
{
    let mutation = command.mutation;
    if host.registry().mutation(&mutation).is_none() {
        return Err(H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::UnknownMutation,
            format!("Unknown mutation \"{mutation}\"."),
            Some(json!({ "mutation": mutation })),
        )));
    }
    let result = host.mutate(&mutation, command.params).await?;
    let accepted = AcceptedMutation { mutation, result };
    let (revision, events) = publish(host, connection, &accepted).await;
    let reply = ProjectionMutationAccepted {
        operation_id: command.operation_id,
        base_revision: command.base_revision,
        revision,
        result: &accepted.result,
    };
    Ok(ProjectionReply {
        result: encode::<H, _>(&reply)?,
        events,
        accepted: Some(accepted),
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

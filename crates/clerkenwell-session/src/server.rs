//! Serving the projection session protocol on one connection.
//!
//! A transport decodes each request into a [`ProjectionCommand`] and hands it
//! to [`serve`] with the connection's [`ProjectionHost`]. It answers the
//! request with the reply's result and sends each of the reply's events as a
//! `projection.update` notification.

use std::future::Future;
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::registry::ProjectionRegistry;
use crate::subscriptions::{ProjectionSubscription, ProjectionSubscriptions};
use crate::transport::{
    ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionMutationAccepted,
    ProjectionMutationCommand, ProjectionOperation, ProjectionResyncCommand,
    ProjectionSubscribeCommand, ProjectionTransportEvent, ProjectionUnsubscribeAccepted,
    ProjectionUnsubscribeCommand, PROJECTION_MUTATE_METHOD, PROJECTION_RESYNC_METHOD,
    PROJECTION_SUBSCRIBE_METHOD, PROJECTION_UNSUBSCRIBE_METHOD,
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
        Some(match method {
            PROJECTION_SUBSCRIBE_METHOD => decode_params(params).map(Self::Subscribe),
            PROJECTION_RESYNC_METHOD => decode_params(params).map(Self::Resync),
            PROJECTION_UNSUBSCRIBE_METHOD => decode_params(params).map(Self::Unsubscribe),
            PROJECTION_MUTATE_METHOD => decode_params(params).map(Self::Mutate),
            _ => return None,
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

/// What an application provides to serve projections on one connection.
pub trait ProjectionHost: Sync {
    type Snapshot: Serialize + Send;
    type Patch: Serialize + Clone + PartialEq + Send;
    /// What a snapshot leaves its client holding, for a projection whose
    /// patches carry only what follows it.
    type Delivery: Clone + Send;
    type MutationResult: Serialize + Send + Sync;
    type Failure: ProjectionFailure + Send;

    /// The projections and mutations served.
    fn registry(&self) -> &ProjectionRegistry;

    /// Runs `transition` on the connection's subscriptions, holding them for
    /// the transition only.
    fn with_subscriptions<R: Send>(
        &self,
        transition: impl FnOnce(&mut ProjectionSubscriptions<Self::Patch, Self::Delivery>) -> R + Send,
    ) -> impl Future<Output = R> + Send;

    fn snapshot(
        &self,
        projection: &str,
        params: &Value,
    ) -> impl Future<Output = Result<Self::Snapshot, Self::Failure>> + Send;

    /// What `snapshot` leaves its client holding, when its projection's
    /// patches carry only what follows it.
    fn delivered(&self, snapshot: &Self::Snapshot) -> Option<Self::Delivery>;

    /// The lowest revision a new subscription to `projection` may start at,
    /// from state that outlives connections.
    fn revision_floor(
        &self,
        projection: &str,
        params: &Value,
    ) -> impl Future<Output = Result<u64, Self::Failure>> + Send;

    /// The lowest revision a mutation's effects may reach, from state that
    /// outlives connections.
    fn mutation_revision_floor(&self) -> impl Future<Output = Result<u64, Self::Failure>> + Send;

    fn mutate(
        &self,
        mutation: &str,
        params: Value,
    ) -> impl Future<Output = Result<Self::MutationResult, Self::Failure>> + Send;

    /// The patch an accepted mutation sends `subscription`, or `None` when its
    /// change reaches that subscription some other way.
    fn mutation_patch(
        &self,
        mutation: &str,
        result: &Self::MutationResult,
        subscription: &ProjectionSubscription<Self::Delivery>,
    ) -> impl Future<Output = Option<Self::Patch>> + Send;
}

/// A served command's result and the updates it sends.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionReply<S, P> {
    /// The accepted command, as the protocol encodes it.
    pub result: Value,
    /// The updates to send the connection's subscriptions, in order.
    pub events: Vec<ProjectionTransportEvent<S, P>>,
}

type Reply<H> = ProjectionReply<<H as ProjectionHost>::Snapshot, <H as ProjectionHost>::Patch>;

/// Serves one command. A failure reaches its client described by a
/// [`ProjectionErrorEnvelope`] addressed to the command.
pub async fn serve<H: ProjectionHost>(
    host: &H,
    command: ProjectionCommand,
) -> Result<Reply<H>, H::Failure> {
    let operation = command.operation();
    let name = command.name().map(str::to_owned);
    let started = Instant::now();
    let outcome = match command {
        ProjectionCommand::Subscribe(command) => subscribe(host, command).await,
        ProjectionCommand::Resync(command) => resync(host, command).await,
        ProjectionCommand::Unsubscribe(command) => unsubscribe(host, command).await,
        ProjectionCommand::Mutate(command) => mutate(host, command).await,
    };
    let outcome = outcome.map_err(|failure| {
        let refusal = failure.describe();
        failure.described(ProjectionErrorEnvelope::new(
            refusal.code,
            refusal.message,
            operation,
            name.as_deref(),
            refusal.details,
        ))
    });
    if operation == ProjectionOperation::Mutate {
        let latency = started.elapsed();
        let rejection = outcome
            .as_ref()
            .err()
            .map(|failure| failure.describe().code.as_str());
        host.with_subscriptions(move |subscriptions| {
            subscriptions.record_mutation(latency, rejection);
        })
        .await;
    }
    outcome
}

async fn subscribe<H: ProjectionHost>(
    host: &H,
    command: ProjectionSubscribeCommand,
) -> Result<Reply<H>, H::Failure> {
    if host.registry().projection(&command.projection).is_none() {
        return Err(H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::UnknownProjection,
            format!("Unknown projection \"{}\".", command.projection),
            Some(json!({ "projection": command.projection })),
        )));
    }
    let params = command.params.unwrap_or_else(|| json!({}));
    let snapshot = host.snapshot(&command.projection, &params).await?;
    let delivered = host.delivered(&snapshot);
    let floor = host.revision_floor(&command.projection, &params).await?;
    let projection = command.projection;
    let cursor = command.cursor.map(|cursor| cursor.revision);
    let (accepted, events) = host
        .with_subscriptions(move |subscriptions| {
            subscriptions
                .accept_subscription(projection, params, cursor, floor, snapshot, delivered)
        })
        .await
        .map_err(|ahead| {
            H::Failure::refused(ProjectionRefusal::new(
                ProjectionErrorCode::CursorAhead,
                format!(
                    "Projection cursor {} is ahead of current revision {}.",
                    ahead.cursor_revision, ahead.current_revision
                ),
                Some(json!({
                    "cursor_revision": ahead.cursor_revision,
                    "current_revision": ahead.current_revision,
                })),
            ))
        })?;
    Ok(ProjectionReply {
        result: encode::<H, _>(&accepted)?,
        events,
    })
}

async fn resync<H: ProjectionHost>(
    host: &H,
    command: ProjectionResyncCommand,
) -> Result<Reply<H>, H::Failure> {
    let subscription_id = command.subscription_id;
    let subscription = host
        .with_subscriptions(move |subscriptions| subscriptions.get(subscription_id).cloned())
        .await
        .ok_or_else(|| subscription_not_found::<H>(subscription_id))?;
    let snapshot = host
        .snapshot(&subscription.projection, &subscription.params)
        .await?;
    let delivered = host.delivered(&snapshot);
    let (accepted, event) = host
        .with_subscriptions(move |subscriptions| {
            subscriptions.resync(subscription_id, snapshot, delivered, "clientRequested")
        })
        .await
        .ok_or_else(|| subscription_not_found::<H>(subscription_id))?;
    Ok(ProjectionReply {
        result: encode::<H, _>(&accepted)?,
        events: vec![event],
    })
}

fn subscription_not_found<H: ProjectionHost>(subscription_id: u64) -> H::Failure {
    H::Failure::refused(ProjectionRefusal::new(
        ProjectionErrorCode::NotFound,
        format!("Projection subscription {subscription_id} does not exist."),
        Some(json!({ "subscription_id": subscription_id })),
    ))
}

async fn unsubscribe<H: ProjectionHost>(
    host: &H,
    command: ProjectionUnsubscribeCommand,
) -> Result<Reply<H>, H::Failure> {
    let removed = host
        .with_subscriptions(move |subscriptions| subscriptions.remove(command.subscription_id))
        .await;
    Ok(ProjectionReply {
        result: encode::<H, _>(&ProjectionUnsubscribeAccepted { removed })?,
        events: Vec::new(),
    })
}

async fn mutate<H: ProjectionHost>(
    host: &H,
    command: ProjectionMutationCommand,
) -> Result<Reply<H>, H::Failure> {
    let registry = host.registry();
    let mutation = command.mutation.as_str();
    if registry.mutation(mutation).is_none() {
        return Err(H::Failure::refused(ProjectionRefusal::new(
            ProjectionErrorCode::UnknownMutation,
            format!("Unknown mutation \"{mutation}\"."),
            Some(json!({ "mutation": mutation })),
        )));
    }
    let result = host.mutate(mutation, command.params).await?;
    let publishes = !registry.plans_only(mutation);
    let floor = host.mutation_revision_floor().await?;
    let revision = host
        .with_subscriptions(move |subscriptions| subscriptions.mutation_revision(floor, publishes))
        .await;
    let mut events = Vec::new();
    if publishes {
        let affected = host
            .with_subscriptions(|subscriptions| {
                let mut affected = subscriptions
                    .all()
                    .iter()
                    .filter(|(_, subscription)| {
                        registry.mutation_affects_projection(mutation, &subscription.projection)
                    })
                    .map(|(subscription_id, subscription)| (*subscription_id, subscription.clone()))
                    .collect::<Vec<_>>();
                affected.sort_by_key(|(subscription_id, _)| *subscription_id);
                affected
            })
            .await;
        for (subscription_id, subscription) in affected {
            let Some(patch) = host.mutation_patch(mutation, &result, &subscription).await else {
                continue;
            };
            let delivered = host
                .with_subscriptions(move |subscriptions| {
                    subscriptions.deliver_patch(subscription_id, revision, patch)
                })
                .await;
            events.extend(delivered);
        }
    }
    let accepted = ProjectionMutationAccepted {
        operation_id: command.operation_id,
        base_revision: command.base_revision,
        revision,
        result,
    };
    Ok(ProjectionReply {
        result: encode::<H, _>(&accepted)?,
        events,
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

//! Wire contracts for projection subscriptions, mutations and commands.
//!
//! Snapshot, patch, mutation-result and command-result payloads are the
//! application's own types, so the accepted-mutation, accepted-command and
//! event contracts are generic over them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROJECTION_SUBSCRIBE_METHOD: &str = "projection.subscribe";
pub const PROJECTION_RESYNC_METHOD: &str = "projection.resync";
pub const PROJECTION_UNSUBSCRIBE_METHOD: &str = "projection.unsubscribe";
pub const PROJECTION_MUTATE_METHOD: &str = "projection.mutate";
pub const PROJECTION_COMMAND_METHOD: &str = "projection.command";
pub const PROJECTION_UPDATE_NOTIFICATION: &str = "projection.update";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSubscribeRequest {
    pub projection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// What an earlier subscription's last update said the client holds. The
    /// subscription then sends only what the client lacks. A value the server
    /// cannot read counts as holding nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionUnsubscribeRequest {
    pub subscription_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionResyncRequest {
    pub subscription_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionMutationRequest {
    pub mutation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<u64>,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionCommandRequest {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSubscribeAccepted {
    pub subscription_id: u64,
    pub revision: u64,
    /// The client already holds what the subscription would send, so no
    /// snapshot follows: it keeps what it holds, at `revision`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub up_to_date: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionUnsubscribeAccepted {
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionResyncAccepted {
    pub subscription_id: u64,
    pub from_revision: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionMutationAccepted<R> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<u64>,
    pub revision: u64,
    pub result: R,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionCommandAccepted<R> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// The revision the command's effects reach on its connection.
    pub revision: u64,
    pub result: R,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectionTransportEvent<S, P> {
    Snapshot {
        subscription_id: u64,
        revision: u64,
        snapshot: S,
        /// What the update leaves the client holding, for it to send back when
        /// it subscribes again. Absent when the server does not name it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        held: Option<Value>,
    },
    Patch {
        subscription_id: u64,
        from_revision: u64,
        to_revision: u64,
        patch: P,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        held: Option<Value>,
    },
}

/// Why a request was refused. `cancelled` work was stopped on purpose before
/// it finished, as when a user cancels a running command; `interrupted` work
/// was stopped by the server program, as when it shuts down.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionErrorCode {
    UnknownMutation,
    UnsupportedMutation,
    UnknownCommand,
    UnsupportedCommand,
    UnknownProjection,
    UnsupportedProjection,
    InvalidParams,
    NotFound,
    Conflict,
    ValidationFailed,
    BaseRevisionInFuture,
    StaleWrite,
    RateLimit,
    Cancelled,
    Interrupted,
    InternalError,
}

impl ProjectionErrorCode {
    pub const ALL: [Self; 16] = [
        Self::UnknownMutation,
        Self::UnsupportedMutation,
        Self::UnknownCommand,
        Self::UnsupportedCommand,
        Self::UnknownProjection,
        Self::UnsupportedProjection,
        Self::InvalidParams,
        Self::NotFound,
        Self::Conflict,
        Self::ValidationFailed,
        Self::BaseRevisionInFuture,
        Self::StaleWrite,
        Self::RateLimit,
        Self::Cancelled,
        Self::Interrupted,
        Self::InternalError,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownMutation => "unknown_mutation",
            Self::UnsupportedMutation => "unsupported_mutation",
            Self::UnknownCommand => "unknown_command",
            Self::UnsupportedCommand => "unsupported_command",
            Self::UnknownProjection => "unknown_projection",
            Self::UnsupportedProjection => "unsupported_projection",
            Self::InvalidParams => "invalid_params",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::ValidationFailed => "validation_failed",
            Self::BaseRevisionInFuture => "base_revision_in_future",
            Self::StaleWrite => "stale_write",
            Self::RateLimit => "rate_limit",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::InternalError => "internal_error",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == code)
    }

    /// Whether the same request may succeed if sent again unchanged.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::BaseRevisionInFuture | Self::RateLimit | Self::Interrupted
        )
    }
}

/// The request a refusal answers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionOperation {
    Subscribe,
    Resync,
    Unsubscribe,
    Mutate,
    Command,
}

impl ProjectionOperation {
    /// The operation a request method names, or `None` when the method is not
    /// part of the protocol.
    pub fn for_method(method: &str) -> Option<Self> {
        match method {
            PROJECTION_SUBSCRIBE_METHOD => Some(Self::Subscribe),
            PROJECTION_RESYNC_METHOD => Some(Self::Resync),
            PROJECTION_UNSUBSCRIBE_METHOD => Some(Self::Unsubscribe),
            PROJECTION_MUTATE_METHOD => Some(Self::Mutate),
            PROJECTION_COMMAND_METHOD => Some(Self::Command),
            _ => None,
        }
    }
}

/// A refused request as its client reads it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionErrorEnvelope {
    pub code: ProjectionErrorCode,
    pub message: String,
    pub operation: ProjectionOperation,
    pub retryable: bool,
    /// The projection, mutation or command the request named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ProjectionErrorEnvelope {
    pub fn new(
        code: ProjectionErrorCode,
        message: impl Into<String>,
        operation: ProjectionOperation,
        name: Option<&str>,
        details: Option<Value>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            operation,
            retryable: code.retryable(),
            name: name.map(str::to_owned),
            details,
        }
    }
}

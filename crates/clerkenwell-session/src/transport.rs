//! Wire contracts for projection subscriptions and mutations.
//!
//! Snapshot, patch and mutation-result payloads are the application's own
//! types, so the accepted-mutation and event contracts are generic over them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROJECTION_SUBSCRIBE_METHOD: &str = "projection.subscribe";
pub const PROJECTION_RESYNC_METHOD: &str = "projection.resync";
pub const PROJECTION_UNSUBSCRIBE_METHOD: &str = "projection.unsubscribe";
pub const PROJECTION_MUTATE_METHOD: &str = "projection.mutate";
pub const PROJECTION_UPDATE_NOTIFICATION: &str = "projection.update";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSubscribeCommand {
    pub projection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProjectionSubscribeCursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSubscribeCursor {
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionUnsubscribeCommand {
    pub subscription_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionResyncCommand {
    pub subscription_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionMutationCommand {
    pub mutation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<u64>,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionSubscribeAccepted {
    pub subscription_id: u64,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ProjectionSubscribeResume>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectionSubscribeResume {
    UpToDate {
        revision: u64,
    },
    Patches {
        from_revision: u64,
        revision: u64,
        patch_count: usize,
    },
    Snapshot {
        from_revision: u64,
        revision: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionUnsubscribeAccepted {
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionResyncAccepted {
    pub subscription_id: u64,
    pub from_revision: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionMutationAccepted<R> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<u64>,
    pub revision: u64,
    pub result: R,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectionTransportEvent<S, P> {
    Snapshot {
        subscription_id: u64,
        revision: u64,
        snapshot: S,
    },
    Patch {
        subscription_id: u64,
        from_revision: u64,
        to_revision: u64,
        patch: P,
    },
}

/// Why a command was refused.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionErrorCode {
    UnknownMutation,
    UnsupportedMutation,
    UnknownProjection,
    UnsupportedProjection,
    InvalidParams,
    NotFound,
    Conflict,
    ValidationFailed,
    BaseRevisionInFuture,
    CursorAhead,
    StaleWrite,
    RateLimit,
    InternalError,
}

impl ProjectionErrorCode {
    pub const ALL: [Self; 13] = [
        Self::UnknownMutation,
        Self::UnsupportedMutation,
        Self::UnknownProjection,
        Self::UnsupportedProjection,
        Self::InvalidParams,
        Self::NotFound,
        Self::Conflict,
        Self::ValidationFailed,
        Self::BaseRevisionInFuture,
        Self::CursorAhead,
        Self::StaleWrite,
        Self::RateLimit,
        Self::InternalError,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownMutation => "unknown_mutation",
            Self::UnsupportedMutation => "unsupported_mutation",
            Self::UnknownProjection => "unknown_projection",
            Self::UnsupportedProjection => "unsupported_projection",
            Self::InvalidParams => "invalid_params",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::ValidationFailed => "validation_failed",
            Self::BaseRevisionInFuture => "base_revision_in_future",
            Self::CursorAhead => "cursor_ahead",
            Self::StaleWrite => "stale_write",
            Self::RateLimit => "rate_limit",
            Self::InternalError => "internal_error",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == code)
    }

    /// Whether the same command may succeed if sent again unchanged.
    pub fn retryable(self) -> bool {
        matches!(self, Self::BaseRevisionInFuture | Self::RateLimit)
    }
}

/// The command a refusal answers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionOperation {
    Subscribe,
    Resync,
    Unsubscribe,
    Mutate,
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
            _ => None,
        }
    }
}

/// A refused command as its client reads it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionErrorEnvelope {
    pub code: ProjectionErrorCode,
    pub message: String,
    pub operation: ProjectionOperation,
    pub retryable: bool,
    /// The projection or mutation the command named.
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

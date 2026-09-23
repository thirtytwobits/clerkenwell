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

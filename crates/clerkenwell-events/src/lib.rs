//! The change-event envelope and the in-process feed that carries it.
//!
//! A store announces each change to a document's accepted state as a
//! [`ChangeEvent`], a CloudEvents 1.0 event whose extensions carry the
//! document's generation, who made the change and why, and the trace it
//! belongs to. The crate depends on nothing collaborative, so an application
//! that only reads events shares the envelope without the store.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::{Arc, RwLock};

/// The CloudEvents specification version every change event follows.
pub const SPEC_VERSION: &str = "1.0";

/// The content type of every change event's data.
pub const DATA_CONTENT_TYPE: &str = "application/json";

/// What a change did to its document: the event's CloudEvents `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    /// A commit created the document or accepted new state for it.
    #[serde(rename = "clerkenwell.document.committed")]
    Committed,
    /// The document was deleted.
    #[serde(rename = "clerkenwell.document.deleted")]
    Deleted,
}

/// The kind of principal that made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
    Service,
    System,
}

/// Who makes a change: an identity the host authenticated, and its kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub id: String,
    pub kind: ActorKind,
}

impl Principal {
    pub fn new(id: impl Into<String>, kind: ActorKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }
}

/// One change to one document, as a CloudEvents 1.0 event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeEvent {
    pub specversion: String,
    /// Unique to this event.
    pub id: String,
    /// The store the document lives in.
    pub source: String,
    #[serde(rename = "type")]
    pub kind: ChangeKind,
    /// The document, as `entity/resource_id`.
    pub subject: String,
    /// When the store accepted the change, in RFC 3339.
    pub time: String,
    pub datacontenttype: String,
    /// The document's generation after the change; for a deletion, the
    /// generation deleted, when the store could read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    /// The principal that made the change.
    pub actorid: String,
    pub actorkind: ActorKind,
    /// Why the change was made, when the commit said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// The state of the proposal the change accepted, when it accepted one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposalstate: Option<String>,
    /// The W3C trace context the change belongs to, when the commit carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    pub data: ChangeData,
}

impl ChangeEvent {
    /// A change `actor` made to the document `data` names.
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        kind: ChangeKind,
        time: impl Into<String>,
        generation: Option<u64>,
        actor: &Principal,
        data: ChangeData,
    ) -> Self {
        Self {
            specversion: SPEC_VERSION.to_string(),
            id: id.into(),
            source: source.into(),
            kind,
            subject: format!("{}/{}", data.entity, data.resource_id),
            time: time.into(),
            datacontenttype: DATA_CONTENT_TYPE.to_string(),
            generation,
            actorid: actor.id.clone(),
            actorkind: actor.kind,
            intent: None,
            proposalstate: None,
            traceparent: None,
            data,
        }
    }
}

/// What a change did to its document's accepted state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeData {
    pub entity: String,
    pub resource_id: String,
    /// The accepted state's etag before the change, when the document existed.
    pub etag_before: Option<String>,
    /// The accepted state's etag after the change, unless it was deleted.
    pub etag_after: Option<String>,
    /// The accepted frontier before the change, when the store read it.
    pub frontier_before: Option<String>,
    /// The accepted frontier after the change, unless it was deleted.
    pub frontier_after: Option<String>,
    /// The operation the commit accepted, when it accepted one.
    pub operation_id: Option<String>,
}

type Listener = Arc<dyn Fn(&ChangeEvent) + Send + Sync>;

/// The in-process feed a store announces its changes on. Every listener hears
/// every announcement. Commits that race may be announced out of generation
/// order, so a listener orders one document's events by generation.
#[derive(Default)]
pub struct ChangeFeed {
    listeners: RwLock<Vec<Listener>>,
}

impl ChangeFeed {
    /// Adds `listener`, which hears every change announced from now on. A
    /// listener runs on the committing thread, so it hands work off rather
    /// than doing it.
    pub fn listen(&self, listener: impl Fn(&ChangeEvent) + Send + Sync + 'static) {
        self.listeners
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::new(listener));
    }

    /// Whether any listener would hear an announcement.
    pub fn is_heard(&self) -> bool {
        !self
            .listeners
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    /// Tells every listener about `event`.
    pub fn announce(&self, event: &ChangeEvent) {
        let listeners = self
            .listeners
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        for listener in listeners {
            listener(event);
        }
    }
}

impl fmt::Debug for ChangeFeed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeFeed")
            .field(
                "listeners",
                &self
                    .listeners
                    .read()
                    .map(|listeners| listeners.len())
                    .unwrap_or_default(),
            )
            .finish()
    }
}

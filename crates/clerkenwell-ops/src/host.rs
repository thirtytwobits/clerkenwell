//! What the application supplies.

use clapham::OAuthContext;
use clerkenwell_axum::RpcFailure;
use clerkenwell_events::Principal;
use clerkenwell_schema::GeneratedTransportValue;
use clerkenwell_session::ProjectionHost;
use serde_json::Value;

/// The application's identity binding and document validation.
pub trait OperationsHost: Send + Sync + 'static {
    /// The principal the verified grant `auth` acts as; `None` refuses the
    /// call as `unauthenticated`.
    fn principal(&self, auth: &OAuthContext) -> Option<Principal>;

    /// Judges `document`, an `entity` document an edit would leave, as the
    /// application's imports judge it.
    fn validate(&self, entity: &str, document: &Value) -> Result<(), Invalid>;
}

/// Why the application refuses a document, as `validation_failed`.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid {
    pub message: String,
    pub details: Option<Value>,
}

impl Invalid {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: None,
        }
    }
}

/// A projection host whose projections, mutations and commands Clerkenwell
/// contributes: one built on the transport types generated from its
/// definition, served by a [`clerkenwell_axum::ProjectionServer`].
pub trait GeneratedHost:
    ProjectionHost<
        Failure = RpcFailure,
        Snapshot: GeneratedTransportValue + 'static,
        Patch: Sync + 'static,
        Delivery: Sync + 'static,
        MutationResult: GeneratedTransportValue + 'static,
        CommandResult: GeneratedTransportValue + 'static,
        Announcement: 'static,
    > + Send
    + Sync
    + 'static
{
}

impl<A> GeneratedHost for A where
    A: ProjectionHost<
            Failure = RpcFailure,
            Snapshot: GeneratedTransportValue + 'static,
            Patch: Sync + 'static,
            Delivery: Sync + 'static,
            MutationResult: GeneratedTransportValue + 'static,
            CommandResult: GeneratedTransportValue + 'static,
            Announcement: 'static,
        > + Send
        + Sync
        + 'static
{
}

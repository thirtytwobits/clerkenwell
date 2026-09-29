//! The projection session protocol.
//!
//! A client subscribes to a projection with optional parameters, receives a
//! snapshot and then patches, and mutates with a client-minted operation id.
//! Every update names what it leaves the client holding; a client that
//! subscribes again with that is sent only what it lacks. This crate holds the
//! wire contracts, the projection registry, and the per-connection
//! subscription state. [`server::serve_request`] answers each request on a
//! connection; producing snapshots and patches is the application's.

mod authoring;
mod registry;
pub mod server;
mod subscriptions;
pub mod transport;

pub use authoring::{AuthoringHeld, AuthoringState, AuthoringStates, Held};
pub use registry::ProjectionRegistry;
pub use server::{
    deliver_change, publish, resync_all, serve, serve_request, AcceptedMutation, ProjectionCommand,
    ProjectionConnection, ProjectionFailure, ProjectionHost, ProjectionRefusal, ProjectionReply,
};
pub use subscriptions::{
    FollowingDelivery, ProjectionRuntimeDiagnostics, ProjectionSubscription,
    ProjectionSubscriptionDiagnostics, ProjectionSubscriptions,
};

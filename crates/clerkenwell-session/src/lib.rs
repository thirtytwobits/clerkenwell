//! The projection session protocol.
//!
//! A client subscribes to a projection with optional parameters and an
//! optional cursor, receives a snapshot or the patches it missed, and mutates
//! with a client-minted operation id. This crate holds the wire contracts, the
//! projection registry, and the per-connection subscription state that decides
//! how a subscription resumes. Producing snapshots and patches is the
//! application's.

mod registry;
mod subscriptions;
pub mod transport;

pub use registry::ProjectionRegistry;
pub use subscriptions::{
    CursorAhead, FollowingDelivery, ProjectionRuntimeDiagnostics, ProjectionSubscription,
    ProjectionSubscriptionDiagnostics, ProjectionSubscriptions, RetainedProjectionPatch,
    SubscribeOutcome, DEFAULT_RETAINED_PATCH_WINDOW,
};

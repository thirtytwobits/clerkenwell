//! The projection session protocol.
//!
//! A client subscribes to a projection with optional parameters, receives a
//! snapshot and then patches, and mutates and runs commands with a
//! client-minted operation id. A change the application makes outside any
//! request, announced on its [`AnnouncementFeed`], reaches the subscriptions
//! it affects as patches.
//! Every update names what it leaves the client holding; a client that
//! subscribes again with that is sent only what it lacks. This crate holds the
//! wire contracts, the projection registry, and the per-connection
//! subscription state. [`server::serve_request`] answers each request on a
//! connection; producing snapshots and patches is the application's.

mod announcements;
mod authoring;
mod modifications;
mod registry;
pub mod server;
mod subscriptions;
#[cfg(feature = "testing")]
pub mod testing;
pub mod transport;

pub use announcements::{Announced, AnnouncementFeed};
pub use authoring::{AuthoringHeld, AuthoringStates, Held};
pub use modifications::{ModificationCounter, Modified, NumberedSnapshot};
pub use registry::ProjectionRegistry;
pub use server::{
    command_reply, decode_request, deliver_announcement, deliver_change, publish, publish_command,
    resync_all, run_command, serve, serve_request, Accepted, ProjectionConnection,
    ProjectionFailure, ProjectionHost, ProjectionRefusal, ProjectionReply, ProjectionRequest,
    RanCommand,
};
pub use subscriptions::{
    FollowingDelivery, ProjectionRuntimeDiagnostics, ProjectionSubscription,
    ProjectionSubscriptionDiagnostics, ProjectionSubscriptions,
};

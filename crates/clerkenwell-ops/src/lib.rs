//! Clerkenwell's operations, contributed to Clapham.
//!
//! The `clerkenwell` namespace reads and edits an application's
//! collaborative documents and recovers its stores' documents. The
//! namespace of the application's definition holds one operation per
//! projection, mutation and command it declares, run by its server.
//!
//! Every operation runs under the caller's verified OAuth grant, as the
//! principal [`OperationsHost::principal`] maps it to; no operation takes a
//! principal as a parameter. Each document operation names a resource policy
//! of its own name, which [`document_policies`] makes from the application's
//! per-document check and the application registers on its
//! `clapham::Configuration`. A composition that registers none is refused.
//!
//! Refusals carry the code the store or the server refused with: a document
//! changed since the etag an edit read it at is refused as
//! `collaboration_revision`, with the accepted document and its etag.

mod documents;
mod generated;
mod host;
mod policies;
mod recovery;
mod refusals;

use std::sync::Arc;

use clapham::{Call, Contribution, Refusal};
use clerkenwell_axum::ProjectionServer;
use clerkenwell_events::Principal;
use clerkenwell_session::AuthoringStates;

pub use host::{GeneratedHost, Invalid, OperationsHost};
pub use policies::{document_policies, DocumentScope, Forbidden};

/// The namespace of Clerkenwell's document and recovery operations.
pub const NAMESPACE: &str = "clerkenwell";

/// The scope a grant needs to list and read documents.
pub const DOCUMENTS_READ: &str = "clerkenwell:documents.read";
/// The scope a grant needs to edit documents.
pub const DOCUMENTS_WRITE: &str = "clerkenwell:documents.write";
/// The scope a grant needs to list stores, verify, export and audit
/// documents.
pub const RECOVERY_READ: &str = "clerkenwell:recovery.read";
/// The scope a grant needs to quarantine, repair and reindex documents.
pub const RECOVERY_ADMIN: &str = "clerkenwell:recovery.admin";
/// The scope a grant needs to reset a document.
pub const RECOVERY_RESET: &str = "clerkenwell:recovery.reset";

/// `clerkenwell.document.get` as an MCP resource template, for the
/// application to register on its MCP server: a document by its entity and
/// id, with the parameters that choose its store, where its entity declares
/// any, as query parameters.
pub const DOCUMENT_RESOURCE: (&str, &str) =
    (documents::GET, "clerkenwell://{entity}/{id}{?store*}");

/// Why a definition entry cannot be an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionError {
    /// The operation's complete name.
    pub operation: String,
    pub reason: String,
}

impl std::fmt::Display for ContributionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} is not an operation: {}",
            self.operation, self.reason
        )
    }
}

impl std::error::Error for ContributionError {}

/// The operations of a running server: the `clerkenwell` namespace's
/// document and recovery operations over the stores its authoring states
/// keep, where it serves any, and an operation per projection, mutation and
/// command of its registry, under its namespace.
pub fn contribute<A: GeneratedHost>(
    server: Arc<ProjectionServer<A>>,
    host: Arc<dyn OperationsHost>,
) -> Result<Vec<Contribution>, ContributionError> {
    let mut contributions = Vec::new();
    if let Some(authoring) = server.application().authoring_states() {
        let clerkenwell = documents::contribute(
            Contribution::new(NAMESPACE),
            authoring.clone(),
            host.clone(),
        );
        contributions.push(recovery::contribute(
            clerkenwell,
            authoring.stores().clone(),
            host.clone(),
        ));
    }
    contributions.push(generated::contribute(server, host)?);
    Ok(contributions)
}

/// The recovery operations over `authoring`'s stores, for a process that
/// holds the workspace under the application's exclusivity and runs no
/// server.
pub fn contribute_held(authoring: AuthoringStates, host: Arc<dyn OperationsHost>) -> Contribution {
    recovery::contribute(
        Contribution::new(NAMESPACE),
        authoring.stores().clone(),
        host,
    )
}

/// The principal `call`'s grant acts as.
fn principal(host: &dyn OperationsHost, call: &Call) -> Result<Principal, Refusal> {
    host.principal(&call.auth)
        .ok_or_else(refusals::unauthenticated)
}

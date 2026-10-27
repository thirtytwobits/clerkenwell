//! The codes Clerkenwell's operations refuse with, and their categories.

use clapham::{Category, Evidence, Refusal};
use clerkenwell_axum::RpcFailure;
use clerkenwell_session::transport::ProjectionErrorCode;
use clerkenwell_session::ProjectionFailure;
use clerkenwell_store::{StoreError, StoreErrorKind};
use serde_json::Value;

use crate::host::Invalid;

/// The codes a document or recovery operation's store refuses with.
pub(crate) const STORE_REFUSALS: &[(&str, Category)] = &[
    ("collaboration_document_not_found", Category::NotFound),
    ("not_found", Category::NotFound),
    ("collaboration_revision", Category::Conflict),
    ("collaboration_policy", Category::Conflict),
    ("collaboration_resync_required", Category::Conflict),
    ("collaboration_dependency_blocked", Category::Conflict),
    ("collaboration_peer_bound", Category::Conflict),
    ("collaboration_migration_required", Category::Conflict),
    ("collaboration_commit_contended", Category::Conflict),
    ("collaboration_repair_contended", Category::Conflict),
    ("conflict", Category::Conflict),
    ("collaboration_text_mismatch", Category::InvalidInput),
    (
        "collaboration_operation_id_collision",
        Category::InvalidInput,
    ),
    ("invalid_collaboration_update", Category::InvalidInput),
    ("invalid_collaboration_version", Category::InvalidInput),
    (
        "unsupported_collaboration_schema_version",
        Category::InvalidInput,
    ),
    ("invalid_params", Category::InvalidInput),
    ("collaboration_state_corrupt", Category::Internal),
    ("internal_error", Category::Internal),
];

/// The codes every operation refuses a caller with before it runs.
pub(crate) const CALLER_REFUSALS: &[(&str, Category)] = &[
    ("unauthenticated", Category::Refused),
    ("forbidden", Category::Refused),
];

/// The code a document the application refuses is refused with.
pub(crate) const VALIDATION_FAILED: (&str, Category) =
    ("validation_failed", Category::InvalidInput);

/// The codes a generated operation's host refuses with.
pub(crate) fn host_refusals() -> impl Iterator<Item = (&'static str, Category)> {
    ProjectionErrorCode::ALL
        .into_iter()
        .filter_map(|code| host_category(code).map(|category| (code.as_str(), category)))
}

/// The category of `code`, refused by a generated operation's host; `None`
/// for a code no operation built from the registry is refused with.
fn host_category(code: ProjectionErrorCode) -> Option<Category> {
    Some(match code {
        ProjectionErrorCode::UnknownMutation
        | ProjectionErrorCode::UnknownCommand
        | ProjectionErrorCode::UnknownProjection => return None,
        ProjectionErrorCode::UnsupportedMutation
        | ProjectionErrorCode::UnsupportedCommand
        | ProjectionErrorCode::UnsupportedProjection => Category::Configuration,
        ProjectionErrorCode::InvalidParams | ProjectionErrorCode::ValidationFailed => {
            Category::InvalidInput
        }
        ProjectionErrorCode::NotFound => Category::NotFound,
        ProjectionErrorCode::Conflict
        | ProjectionErrorCode::StaleWrite
        | ProjectionErrorCode::BaseRevisionInFuture => Category::Conflict,
        ProjectionErrorCode::RateLimit => Category::Unavailable,
        ProjectionErrorCode::Cancelled | ProjectionErrorCode::Interrupted => Category::Interrupted,
        ProjectionErrorCode::InternalError => Category::Internal,
    })
}

/// A store's refusal, carrying the code its data names: for a conflict, the
/// kind of conflict, as the store's audit records it. A refusal whose data
/// names no code carries the one its kind implies.
pub(crate) fn store(error: StoreError) -> Refusal {
    let data = error.data.as_ref();
    let code = data
        .and_then(|data| data["code"].as_str())
        .map(
            |code| match (code, data.and_then(|data| data["conflict_kind"].as_str())) {
                ("conflict", Some(kind)) => kind.to_owned(),
                _ => code.to_owned(),
            },
        )
        .unwrap_or_else(|| {
            match error.kind {
                StoreErrorKind::NotFound => "not_found",
                StoreErrorKind::Conflict => "conflict",
                StoreErrorKind::InvalidRequest => "invalid_params",
                StoreErrorKind::Internal => "internal_error",
            }
            .to_owned()
        });
    let category = STORE_REFUSALS
        .iter()
        .find(|(declared, _)| *declared == code)
        .map_or(Category::Internal, |(_, category)| *category);
    let retryable = matches!(
        code.as_str(),
        "collaboration_commit_contended" | "collaboration_repair_contended"
    );
    let mut refusal = Refusal::new(code, category, error.message);
    refusal.details = error.data;
    refusal.retryable = retryable;
    refusal
}

/// A refusal of a request the store did not run: what it names is absent.
pub(crate) fn not_found(message: impl Into<String>, details: Value) -> Refusal {
    let mut refusal = Refusal::not_dispatched("not_found", Category::NotFound, message);
    refusal.details = Some(details);
    refusal
}

/// A refusal of a request the store did not run: its parameters are wrong.
pub(crate) fn invalid_params(message: impl Into<String>, details: Value) -> Refusal {
    let mut refusal = Refusal::not_dispatched("invalid_params", Category::InvalidInput, message);
    refusal.details = Some(details);
    refusal
}

/// The refusal of a document the application judged invalid.
pub(crate) fn invalid(invalid: Invalid) -> Refusal {
    let (code, category) = VALIDATION_FAILED;
    let mut refusal = Refusal::new(code, category, invalid.message);
    refusal.details = invalid.details;
    refusal
}

/// The refusal of a grant the application's identity binding maps to no
/// principal.
pub(crate) fn unauthenticated() -> Refusal {
    Refusal::not_dispatched(
        "unauthenticated",
        Category::Refused,
        "The grant acts as no principal of the application.",
    )
}

/// A generated operation's host refusal, carrying its protocol code. A
/// refusal the server makes before it runs a command was not dispatched; an
/// interrupted one may have had its effect.
pub(crate) fn host(failure: RpcFailure) -> Refusal {
    let refused = failure.describe();
    let category = host_category(refused.code).unwrap_or(Category::Internal);
    let mut refusal = Refusal::new(refused.code.as_str(), category, refused.message);
    refusal.details = refused.details;
    match refused.code {
        ProjectionErrorCode::RateLimit => {
            refusal.retryable = true;
            refusal.evidence = Evidence::NotDispatched;
        }
        ProjectionErrorCode::Interrupted => {
            refusal.retryable = true;
            refusal.evidence = Evidence::Uncertain;
        }
        _ => {}
    }
    refusal
}

/// The refusal of a call cancelled while its command runs: the command runs
/// to its end.
pub(crate) fn cancelled_while_running(command: &str) -> Refusal {
    Refusal::new(
        "interrupted",
        Category::Interrupted,
        format!("The call was cancelled while \"{command}\" ran; the command runs to its end."),
    )
    .with_evidence(Evidence::Uncertain)
}

/// The refusal of a result its operation's schema cannot describe.
pub(crate) fn unencodable(name: &str, error: serde_json::Error) -> Refusal {
    Refusal::new(
        "internal_error",
        Category::Internal,
        format!("The result of \"{name}\" could not be encoded: {error}"),
    )
}

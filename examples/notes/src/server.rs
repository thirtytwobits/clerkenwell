//! The notes definition served over the projection protocol.
//!
//! A client creates notes, subscribes to a note's authoring state and sends
//! the operations its replica recorded. The session delivers every note's
//! authoring state from the store, and every subscriber to a note takes its
//! new authoring state when any client's operations are accepted.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use clerkenwell_axum::RpcFailure;
use clerkenwell_session::transport::ProjectionErrorCode;
use clerkenwell_session::{
    AuthoringStates, Held, ProjectionFailure, ProjectionHost, ProjectionRefusal,
    ProjectionRegistry, ProjectionSubscription,
};
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationService, CollaborationStores, ImportFence, LocalFileCollaborationStorage,
    StoreError,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::model::{
    NoteCreateParams, NoteMutationResult, ProjectionTransportMutationResult,
    ProjectionTransportPatch, ProjectionTransportSnapshot, ReplicaUpdateParams,
    ReplicaUpdateParamsExchangeMode, ReplicaUpdateResult, GENERATED_COLLABORATION_SPECS,
    GENERATED_ENTITY_AUTHORING_SPECS, GENERATED_MUTATION_SPECS, GENERATED_PROJECTION_SPECS,
    NOTE_CREATE_MUTATION, NOTE_IMPORT_UPDATE_MUTATION,
};
use crate::{validate, COMMIT_POLICY, NOTE};

/// Mutations, or changes to notes, a connection of the example's server may
/// fall behind before it resynchronises.
pub const PUBLICATION_WINDOW: NonZeroUsize = match NonZeroUsize::new(256) {
    Some(window) => window,
    None => panic!("the publication window is non-zero"),
};

/// The store that keeps every note.
pub const NOTES_STORE: &str = "notes";

static REGISTRY: ProjectionRegistry = ProjectionRegistry::new(
    GENERATED_PROJECTION_SPECS,
    GENERATED_MUTATION_SPECS,
    GENERATED_ENTITY_AUTHORING_SPECS,
);

/// Notes kept in a store, served to every connection.
pub struct NotesServer {
    service: CollaborationService,
    authoring: AuthoringStates,
    created: AtomicU64,
}

impl NotesServer {
    /// Notes kept in a store under `root`.
    pub fn new(root: &Path) -> Self {
        let stores = CollaborationStores::new(GENERATED_COLLABORATION_SPECS, COMMIT_POLICY);
        let service = stores
            .register(NOTES_STORE, LocalFileCollaborationStorage::new(root))
            .expect("a new set holds no other store");
        Self {
            service,
            authoring: AuthoringStates::new(stores, GENERATED_COLLABORATION_SPECS, |_, _| {
                Some(NOTES_STORE.to_owned())
            }),
            created: AtomicU64::new(1),
        }
    }

    fn create(&self, params: NoteCreateParams) -> Result<NoteMutationResult, RpcFailure> {
        let note_id = format!("note-{}", self.created.fetch_add(1, Ordering::Relaxed));
        let seed = json!({ "title": params.title, "body": "", "status": "draft" });
        self.service
            .bootstrap(NOTE, &note(&note_id), &seed, validate)
            .map_err(refused)?;
        let etag = self
            .service
            .authoring_state(NOTE, &note(&note_id), None)
            .map_err(refused)?
            .etag;
        Ok(NoteMutationResult { note_id, etag })
    }

    fn import(&self, params: ReplicaUpdateParams) -> Result<ReplicaUpdateResult, RpcFailure> {
        let document = note(&params.note_id);
        match params.exchange_mode {
            ReplicaUpdateParamsExchangeMode::Bootstrap => {
                self.service
                    .bootstrap_update(
                        NOTE,
                        &document,
                        NOTE.schema_version,
                        &params.update_base64,
                        validate,
                    )
                    .map_err(refused)?;
                let state = self
                    .service
                    .authoring_state(NOTE, &document, None)
                    .map_err(refused)?;
                Ok(ReplicaUpdateResult {
                    note_id: params.note_id,
                    etag: state.etag,
                    accepted_frontier_base64: state.accepted_frontier_base64,
                    missing_update_base64: state.update_base64,
                })
            }
            ReplicaUpdateParamsExchangeMode::Incremental => {
                let base_frontier_base64 = params.base_frontier_base64.ok_or_else(|| {
                    RpcFailure::new(
                        ProjectionErrorCode::InvalidParams,
                        "An incremental update names the frontier it extends.",
                        None,
                    )
                })?;
                let imported = self
                    .service
                    .import(
                        NOTE,
                        CollaborationImportRequest {
                            document,
                            schema_version: NOTE.schema_version,
                            operation_id: params.operation_id,
                            exchange_mode: CollaborationExchangeMode::Incremental,
                            base_frontier_base64,
                            update_base64: params.update_base64,
                            fence: ImportFence::Frontier,
                        },
                        validate,
                    )
                    .map_err(refused)?;
                Ok(ReplicaUpdateResult {
                    note_id: params.note_id,
                    etag: imported.etag,
                    accepted_frontier_base64: imported.accepted_frontier_base64,
                    missing_update_base64: imported.missing_update_base64,
                })
            }
        }
    }
}

impl ProjectionHost for NotesServer {
    type Snapshot = ProjectionTransportSnapshot;
    type Patch = ProjectionTransportPatch;
    type Delivery = ();
    type MutationResult = ProjectionTransportMutationResult;
    type Failure = RpcFailure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    fn authoring_states(&self) -> Option<&AuthoringStates> {
        Some(&self.authoring)
    }

    /// Every projection the notes definition declares is an authoring state.
    async fn snapshot(
        &self,
        projection: &str,
        _params: &Value,
        _held: Option<&()>,
    ) -> Result<ProjectionTransportSnapshot, RpcFailure> {
        Err(RpcFailure::new(
            ProjectionErrorCode::UnsupportedProjection,
            format!("The notes server does not serve \"{projection}\"."),
            None,
        ))
    }

    fn delivered(&self, _snapshot: &ProjectionTransportSnapshot) -> Option<()> {
        None
    }

    fn delivered_by_patch(&self, _patch: &ProjectionTransportPatch) -> Option<()> {
        None
    }

    async fn mutate(
        &self,
        mutation: &str,
        params: Value,
    ) -> Result<ProjectionTransportMutationResult, RpcFailure> {
        match mutation {
            NOTE_CREATE_MUTATION => self
                .create(decode(params)?)
                .map(ProjectionTransportMutationResult::NoteCreate),
            NOTE_IMPORT_UPDATE_MUTATION => self
                .import(decode(params)?)
                .map(ProjectionTransportMutationResult::NoteImportUpdate),
            other => Err(RpcFailure::new(
                ProjectionErrorCode::UnsupportedMutation,
                format!("The notes server does not run \"{other}\"."),
                None,
            )),
        }
    }

    async fn mutation_patch(
        &self,
        _mutation: &str,
        _result: &ProjectionTransportMutationResult,
        _subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<ProjectionTransportPatch> {
        None
    }
}

fn note(note_id: &str) -> CollaborationDocumentId {
    CollaborationDocumentId::new(NOTE.name, note_id)
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T, RpcFailure> {
    serde_json::from_value(params).map_err(|error| {
        RpcFailure::new(ProjectionErrorCode::InvalidParams, error.to_string(), None)
    })
}

fn refused(error: StoreError) -> RpcFailure {
    RpcFailure::refused(ProjectionRefusal::from(error))
}

//! The notes definition served over the projection protocol.
//!
//! A client creates notes, subscribes to a note's authoring state and sends
//! the operations its replica recorded. Every subscriber to a note takes its
//! new authoring state when any client's operations are accepted.

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use clerkenwell_axum::{RpcFailure, ServerWindows};
use clerkenwell_session::transport::ProjectionErrorCode;
use clerkenwell_session::{ProjectionHost, ProjectionRegistry, ProjectionSubscription};
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationService, ImportFence, StoreError, StoreErrorKind,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::model::{
    LoroUpdateParams, LoroUpdateParamsExchangeMode, LoroUpdateResult, NoteAuthoringState,
    NoteAuthoringStateExchangeModes, NoteAuthoringStatePatch, NoteAuthoringStatePatchKind,
    NoteCreateParams, NoteKeyParams, NoteMutationResult, ProjectionTransportMutationResult,
    ProjectionTransportPatch, ProjectionTransportSnapshot, GENERATED_ENTITY_AUTHORING_SPECS,
    GENERATED_MUTATION_SPECS, GENERATED_PROJECTION_SPECS, NOTE_CREATE_MUTATION,
    NOTE_IMPORT_LORO_UPDATE_MUTATION,
};
use crate::{validate, COMMIT_POLICY, NOTE};

/// How far the example's server lets a connection fall behind.
pub const SERVER_WINDOWS: ServerWindows = ServerWindows {
    publications: match NonZeroUsize::new(256) {
        Some(publications) => publications,
        None => panic!("the publication window is non-zero"),
    },
    retained_patches: 64,
};

static REGISTRY: ProjectionRegistry = ProjectionRegistry::new(
    GENERATED_PROJECTION_SPECS,
    GENERATED_MUTATION_SPECS,
    GENERATED_ENTITY_AUTHORING_SPECS,
);

/// Notes kept in a store, served to every connection.
pub struct NotesServer {
    service: CollaborationService,
    created: AtomicU64,
}

impl NotesServer {
    /// Notes kept in a store under `root`.
    pub fn new(root: &Path) -> Self {
        Self {
            service: CollaborationService::new(
                root,
                crate::model::GENERATED_COLLABORATION_SPECS,
                COMMIT_POLICY,
            ),
            created: AtomicU64::new(1),
        }
    }

    fn authoring_state(&self, note_id: &str) -> Result<NoteAuthoringState, RpcFailure> {
        let read = self
            .service
            .authoring_state(NOTE, &note(note_id), None)
            .map_err(refused)?;
        Ok(NoteAuthoringState {
            note_id: note_id.to_owned(),
            etag: read.etag,
            exchange_modes: vec![
                NoteAuthoringStateExchangeModes::Incremental,
                NoteAuthoringStateExchangeModes::Bootstrap,
            ],
            accepted_frontier_base64: read.accepted_frontier_base64,
            update_base64: read.update_base64,
        })
    }

    fn create(&self, params: NoteCreateParams) -> Result<NoteMutationResult, RpcFailure> {
        let note_id = format!("note-{}", self.created.fetch_add(1, Ordering::Relaxed));
        let seed = json!({ "title": params.title, "body": "", "status": "draft" });
        self.service
            .bootstrap(
                NOTE,
                &note(&note_id),
                &relative_path(&note_id),
                &seed,
                validate,
            )
            .map_err(refused)?;
        let etag = self.authoring_state(&note_id)?.etag;
        Ok(NoteMutationResult { note_id, etag })
    }

    fn import(&self, params: LoroUpdateParams) -> Result<LoroUpdateResult, RpcFailure> {
        let document = note(&params.note_id);
        match params.exchange_mode {
            LoroUpdateParamsExchangeMode::Bootstrap => {
                self.service
                    .bootstrap_update(
                        NOTE,
                        &document,
                        &relative_path(&params.note_id),
                        NOTE.schema_version,
                        &params.update_base64,
                        validate,
                    )
                    .map_err(refused)?;
                let state = self.authoring_state(&params.note_id)?;
                Ok(LoroUpdateResult {
                    note_id: params.note_id,
                    etag: state.etag,
                    accepted_frontier_base64: state.accepted_frontier_base64,
                    missing_update_base64: state.update_base64,
                })
            }
            LoroUpdateParamsExchangeMode::Incremental => {
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
                            relative_path: relative_path(&params.note_id),
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
                Ok(LoroUpdateResult {
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

    async fn snapshot(
        &self,
        _projection: &str,
        params: &Value,
    ) -> Result<ProjectionTransportSnapshot, RpcFailure> {
        let params: NoteKeyParams = decode(params.clone())?;
        Ok(ProjectionTransportSnapshot::NotesAuthoringState(
            self.authoring_state(&params.note_id)?,
        ))
    }

    fn delivered(&self, _snapshot: &ProjectionTransportSnapshot) -> Option<()> {
        None
    }

    async fn revision_floor(&self, _projection: &str, _params: &Value) -> Result<u64, RpcFailure> {
        Ok(0)
    }

    async fn mutation_revision_floor(&self) -> Result<u64, RpcFailure> {
        Ok(0)
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
            NOTE_IMPORT_LORO_UPDATE_MUTATION => self
                .import(decode(params)?)
                .map(ProjectionTransportMutationResult::NoteImportLoroUpdate),
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
        result: &ProjectionTransportMutationResult,
        subscription: &ProjectionSubscription<()>,
    ) -> Option<ProjectionTransportPatch> {
        let note_id = match result {
            ProjectionTransportMutationResult::NoteCreate(created) => &created.note_id,
            ProjectionTransportMutationResult::NoteImportLoroUpdate(imported) => &imported.note_id,
        };
        if subscription.params["note_id"].as_str() != Some(note_id) {
            return None;
        }
        let state = self.authoring_state(note_id).ok()?;
        Some(ProjectionTransportPatch::NotesAuthoringState(
            NoteAuthoringStatePatch {
                kind: NoteAuthoringStatePatchKind::Replace,
                state: Some(state),
            },
        ))
    }
}

fn note(note_id: &str) -> CollaborationDocumentId {
    CollaborationDocumentId::new(NOTE.name, note_id)
}

fn relative_path(note_id: &str) -> String {
    format!("notes/{note_id}.json")
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T, RpcFailure> {
    serde_json::from_value(params).map_err(|error| {
        RpcFailure::new(ProjectionErrorCode::InvalidParams, error.to_string(), None)
    })
}

/// A store's refusal as the protocol reports it: the code its data names, or
/// the one its kind implies.
fn refused(error: StoreError) -> RpcFailure {
    let named = error
        .data
        .as_ref()
        .and_then(|data| data["code"].as_str())
        .and_then(ProjectionErrorCode::parse);
    let code = named.unwrap_or(match error.kind {
        StoreErrorKind::NotFound => ProjectionErrorCode::NotFound,
        StoreErrorKind::Conflict => ProjectionErrorCode::Conflict,
        StoreErrorKind::InvalidRequest => ProjectionErrorCode::InvalidParams,
        StoreErrorKind::Internal => ProjectionErrorCode::InternalError,
    });
    RpcFailure::new(code, error.message, error.data)
}

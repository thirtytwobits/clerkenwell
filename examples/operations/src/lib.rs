//! Clerkenwell's operations over a store of tasks, assembled into Clapham's
//! CLI and MCP server.
//!
//! The tasks definition, `tasks.projections.json`, declares a collaborative
//! task with a title, notes whose concurrent edits merge and a status; a
//! projection listing every task; a mutation creating one; and a command
//! closing every open one. `model` holds the bindings generated from it,
//! `Tasks` serves them, and `operations` contributes Clerkenwell's
//! operations over them to Clapham. The binary serves the composition as
//! Clapham's reference process protocol describes, and the tests run it
//! through Clapham's conformance harness.

pub mod conformance;
pub mod model;
pub mod operations;

use std::convert::Infallible;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use clerkenwell_axum::RpcFailure;
use clerkenwell_doc::CollaborationReplica;
use clerkenwell_events::Principal;
use clerkenwell_schema::CollaborationExchangeMode;
use clerkenwell_session::transport::ProjectionErrorCode;
use clerkenwell_session::{
    AuthoringStates, Held, ModificationCounter, Modified, NumberedSnapshot, ProjectionFailure,
    ProjectionHost, ProjectionRefusal, ProjectionRegistry, ProjectionSubscription,
};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentEdit, CollaborationDocumentId, CollaborationImportRequest,
    CollaborationService, CollaborationStoragePort, CollaborationStores, CommitPolicy, ImportFence,
    PeerKey, StoreError,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use model::{
    GeneratedCollaborationEntitySpec, ProjectionTransportCommandResult,
    ProjectionTransportMutationResult, ProjectionTransportPatch, ProjectionTransportSnapshot,
    ReplicaUpdateParams, ReplicaUpdateResult, TaskCloseAllResult, TaskCreateParams,
    TaskCreateResult, TaskStatus, TaskSummary, TasksListPatch, TasksListPatchKind,
    TasksListSnapshot, GENERATED_COLLABORATION_SPECS, GENERATED_COMMAND_SPECS,
    GENERATED_ENTITY_AUTHORING_SPECS, GENERATED_MUTATION_SPECS, GENERATED_PROJECTION_SPECS,
    NAMESPACE, TASKS_LIST_PROJECTION, TASK_CLOSE_ALL_COMMAND, TASK_CREATE_MUTATION,
    TASK_IMPORT_UPDATE_MUTATION,
};

/// The task's collaboration plan.
pub const TASK: &GeneratedCollaborationEntitySpec = &model::TASK_COLLABORATION_SPEC;

/// The store that keeps every task.
pub const TASKS_STORE: &str = "tasks";

/// Publications a connection may fall behind before it resynchronises.
pub const PUBLICATION_WINDOW: NonZeroUsize = match NonZeroUsize::new(256) {
    Some(window) => window,
    None => panic!("the publication window is non-zero"),
};

/// Commands the server runs at once.
pub const COMMAND_LIMIT: NonZeroUsize = match NonZeroUsize::new(16) {
    Some(limit) => limit,
    None => panic!("the command limit is non-zero"),
};

/// How the store commits.
pub const COMMIT_POLICY: CommitPolicy = CommitPolicy {
    retained_operations: 8,
    attempts: match NonZeroU32::new(8) {
        Some(attempts) => attempts,
        None => panic!("attempts are non-zero"),
    },
    backoff: Duration::from_millis(2),
};

static REGISTRY: ProjectionRegistry = ProjectionRegistry::new(
    NAMESPACE,
    GENERATED_PROJECTION_SPECS,
    GENERATED_MUTATION_SPECS,
    GENERATED_COMMAND_SPECS,
    GENERATED_ENTITY_AUTHORING_SPECS,
);

/// The key the store allocates peers from. An application keeps its key
/// secret, as configuration.
pub fn peer_key() -> PeerKey {
    PeerKey::new(b"the operations example's peer key").expect("a key of 32 bytes")
}

/// Refuses a task without a title or with a status the definition does not
/// declare.
pub fn validate(task: &Value) -> Result<(), StoreError> {
    if task["title"].as_str().is_none_or(str::is_empty) {
        return Err(StoreError::invalid_request("A task needs a title."));
    }
    serde_json::from_value::<TaskStatus>(task["status"].clone())
        .map(|_| ())
        .map_err(|_| StoreError::invalid_request(format!("{} is not a status.", task["status"])))
}

/// Tasks kept in a store, and the modifications numbered as they are saved.
pub struct Tasks {
    service: CollaborationService,
    authoring: AuthoringStates,
    modifications: ModificationCounter,
    /// Held while a modification is saved and numbered, and while a snapshot
    /// reads the tasks and the number of the last modification they hold.
    saving: Mutex<()>,
    created: AtomicU64,
}

impl Tasks {
    /// A new store holding a task for each of `seeded`'s ids and titles, each
    /// open and without notes, written by the store itself.
    pub fn seeded(seeded: &[(&str, &str)]) -> Self {
        let tasks = Self::kept_in(MemoryStorage::default());
        for (id, title) in seeded {
            tasks
                .service
                .bootstrap(
                    TASK,
                    &task(id),
                    &json!({ "task_id": id, "etag": "", "title": title, "notes": "", "status": "open" }),
                    validate,
                )
                .expect("a seeded task is valid");
        }
        tasks
    }

    /// A new store holding each of `envelopes`: a task's id and the stored
    /// envelope of its document.
    pub fn stored<'a>(envelopes: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        let storage = MemoryStorage::default();
        for (id, envelope) in envelopes {
            storage
                .compare_and_swap(&task(id), None, envelope)
                .expect("memory storage keeps an envelope")
                .expect("a new store holds no task");
        }
        Self::kept_in(storage)
    }

    /// The tasks `storage` keeps, served from the one store.
    fn kept_in(storage: MemoryStorage) -> Self {
        let stores =
            CollaborationStores::new(GENERATED_COLLABORATION_SPECS, COMMIT_POLICY, peer_key());
        let service = stores
            .register(TASKS_STORE, storage)
            .expect("a new set holds no other store");
        Self {
            service,
            authoring: AuthoringStates::new(stores, GENERATED_COLLABORATION_SPECS, |_, _| {
                Some(TASKS_STORE.to_owned())
            }),
            modifications: ModificationCounter::default(),
            saving: Mutex::new(()),
            created: AtomicU64::new(1),
        }
    }

    /// The store every task is kept in.
    pub fn service(&self) -> &CollaborationService {
        &self.service
    }

    /// Every task, in id order.
    fn list(&self) -> Result<Vec<TaskSummary>, RpcFailure> {
        let mut tasks = Vec::new();
        for summary in self.service.summaries(TASK.name).map_err(refused)? {
            let Some(read) = self
                .service
                .read_document(TASK, &summary.document)
                .map_err(refused)?
            else {
                continue;
            };
            tasks.push(decode(json!({
                "task_id": summary.document.resource_id,
                "title": read.document["title"],
                "status": read.document["status"],
            }))?);
        }
        Ok(tasks)
    }

    /// Creates an open task `principal` titles, written under a block of
    /// peers allocated to it.
    fn create(
        &self,
        principal: &Principal,
        params: TaskCreateParams,
    ) -> Result<TaskCreateResult, RpcFailure> {
        let task_id = format!("task-new-{}", self.created.fetch_add(1, Ordering::Relaxed));
        let document = task(&task_id);
        let seed = json!({ "title": params.title, "notes": "", "status": "open" });
        let block = self.service.allocate_peers(principal, &document);
        let created = CollaborationReplica::from_document(TASK, &seed, block.base)
            .and_then(|replica| replica.export_update_base64())
            .map_err(|error| {
                RpcFailure::new(ProjectionErrorCode::InvalidParams, error.to_string(), None)
            })?;
        let imported = self
            .service
            .import(
                TASK,
                CollaborationImportRequest {
                    document,
                    schema_version: TASK.schema_version,
                    operation_id: format!("create-{task_id}"),
                    exchange_mode: CollaborationExchangeMode::Bootstrap,
                    base_frontier_base64: String::new(),
                    update_base64: created,
                    fence: ImportFence::Frontier,
                    actor: principal.clone(),
                    peer_nonces: vec![block.nonce],
                    intent: None,
                },
                validate,
            )
            .map_err(refused)?;
        Ok(TaskCreateResult {
            task_id,
            etag: imported.etag,
        })
    }

    /// Commits `principal`'s operations: a new task, or edits to one.
    fn import(
        &self,
        principal: &Principal,
        params: ReplicaUpdateParams,
    ) -> Result<ReplicaUpdateResult, RpcFailure> {
        let base_frontier_base64 = match params.exchange_mode {
            CollaborationExchangeMode::Bootstrap => String::new(),
            CollaborationExchangeMode::Incremental => {
                params.base_frontier_base64.ok_or_else(|| {
                    RpcFailure::new(
                        ProjectionErrorCode::InvalidParams,
                        "An incremental update names the frontier it extends.",
                        None,
                    )
                })?
            }
        };
        let imported = self
            .service
            .import(
                TASK,
                CollaborationImportRequest {
                    document: task(&params.task_id),
                    schema_version: TASK.schema_version,
                    operation_id: params.operation_id,
                    exchange_mode: params.exchange_mode,
                    base_frontier_base64,
                    update_base64: params.update_base64,
                    fence: ImportFence::Frontier,
                    actor: principal.clone(),
                    peer_nonces: params.peer_nonces,
                    intent: params.intent,
                },
                validate,
            )
            .map_err(refused)?;
        Ok(ReplicaUpdateResult {
            task_id: params.task_id,
            etag: imported.etag,
            accepted_frontier_base64: imported.accepted_frontier_base64,
            missing_update_base64: imported.missing_update_base64,
        })
    }

    /// Closes every open task as `principal`, and returns how many it closed.
    fn close_all(&self, principal: &Principal) -> Result<TaskCloseAllResult, RpcFailure> {
        let mut closed = 0;
        for summary in self.service.summaries(TASK.name).map_err(refused)? {
            let Some(read) = self
                .service
                .read_document(TASK, &summary.document)
                .map_err(refused)?
            else {
                continue;
            };
            if read.document["status"] != "open" {
                continue;
            }
            let mut edited = read.document.clone();
            edited["status"] = Value::from("closed");
            self.service
                .edit(
                    TASK,
                    CollaborationDocumentEdit {
                        operation_id: format!("close-{}", read.summary.etag),
                        document: summary.document,
                        edited,
                        etag: read.summary.etag,
                        actor: principal.clone(),
                        intent: Some("close every open task".to_owned()),
                    },
                    validate,
                )
                .map_err(refused)?;
            closed += 1;
        }
        Ok(TaskCloseAllResult { closed })
    }

    /// The patch that brings a subscription to the task list to what the
    /// store holds now.
    fn list_patch(&self) -> Option<ProjectionTransportPatch> {
        self.list().ok().map(|tasks| {
            ProjectionTransportPatch::TasksList(TasksListPatch {
                kind: TasksListPatchKind::Reset,
                tasks,
            })
        })
    }
}

impl ProjectionHost for Tasks {
    type Snapshot = ProjectionTransportSnapshot;
    type Patch = ProjectionTransportPatch;
    type Delivery = ();
    type MutationResult = ProjectionTransportMutationResult;
    type CommandResult = ProjectionTransportCommandResult;
    type Announcement = Infallible;
    type Failure = RpcFailure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    fn authoring_states(&self) -> Option<&AuthoringStates> {
        Some(&self.authoring)
    }

    fn modifications(&self) -> &ModificationCounter {
        &self.modifications
    }

    async fn snapshot(
        &self,
        projection: &str,
        _params: &Value,
        _held: Option<&()>,
    ) -> Result<NumberedSnapshot<ProjectionTransportSnapshot>, RpcFailure> {
        if projection != TASKS_LIST_PROJECTION {
            return Err(RpcFailure::new(
                ProjectionErrorCode::UnsupportedProjection,
                format!("The tasks server does not serve \"{projection}\"."),
                None,
            ));
        }
        let _saving = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(NumberedSnapshot {
            snapshot: ProjectionTransportSnapshot::TasksList(TasksListSnapshot {
                tasks: self.list()?,
            }),
            through: self.modifications.last(),
        })
    }

    fn delivered(&self, _snapshot: &ProjectionTransportSnapshot) -> Option<()> {
        None
    }

    fn delivered_by_patch(&self, _patch: &ProjectionTransportPatch) -> Option<()> {
        None
    }

    async fn mutate(
        &self,
        principal: &Principal,
        mutation: &str,
        params: Value,
    ) -> Result<Modified<ProjectionTransportMutationResult>, RpcFailure> {
        let _saving = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        let result = match mutation {
            TASK_CREATE_MUTATION => self
                .create(principal, decode(params)?)
                .map(ProjectionTransportMutationResult::TaskCreate),
            TASK_IMPORT_UPDATE_MUTATION => self
                .import(principal, decode(params)?)
                .map(ProjectionTransportMutationResult::TaskImportUpdate),
            other => Err(RpcFailure::new(
                ProjectionErrorCode::UnsupportedMutation,
                format!("The tasks server does not run \"{other}\"."),
                None,
            )),
        }?;
        Ok(Modified {
            result,
            modification: Some(self.modifications.take()),
        })
    }

    async fn mutation_patch(
        &self,
        _mutation: &str,
        _result: &ProjectionTransportMutationResult,
        _subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<ProjectionTransportPatch> {
        self.list_patch()
    }

    async fn command(
        &self,
        principal: &Principal,
        command: &str,
        _params: Value,
    ) -> Result<Modified<ProjectionTransportCommandResult>, RpcFailure> {
        if command != TASK_CLOSE_ALL_COMMAND {
            return Err(RpcFailure::new(
                ProjectionErrorCode::UnsupportedCommand,
                format!("The tasks server does not run \"{command}\"."),
                None,
            ));
        }
        let _saving = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        let result = self.close_all(principal)?;
        Ok(Modified {
            result: ProjectionTransportCommandResult::TaskCloseAll(result),
            modification: Some(self.modifications.take()),
        })
    }

    async fn command_patch(
        &self,
        _command: &str,
        _result: &ProjectionTransportCommandResult,
        _subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<ProjectionTransportPatch> {
        self.list_patch()
    }

    /// The tasks server announces no change of its own.
    async fn announcement_patch(
        &self,
        announcement: &Infallible,
        _subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<ProjectionTransportPatch> {
        match *announcement {}
    }
}

/// The document of the task `task_id`.
pub fn task(task_id: &str) -> CollaborationDocumentId {
    CollaborationDocumentId::new(TASK.name, task_id)
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T, RpcFailure> {
    serde_json::from_value(params).map_err(|error| {
        RpcFailure::new(ProjectionErrorCode::InvalidParams, error.to_string(), None)
    })
}

fn refused(error: StoreError) -> RpcFailure {
    RpcFailure::refused(ProjectionRefusal::from(error))
}

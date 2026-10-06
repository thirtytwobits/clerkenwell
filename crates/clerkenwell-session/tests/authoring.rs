//! Authoring states answered and delivered by the session from the stores an
//! application keeps its collaborative documents in.

use std::future::Future;
use std::num::NonZeroU32;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use clerkenwell_doc::CollaborationReplica;
use clerkenwell_events::{ActorKind, ChangeEvent, Principal};
use clerkenwell_schema::{
    GeneratedCollaborationConflict, GeneratedCollaborationEntitySpec,
    GeneratedCollaborationFieldSpec, GeneratedCollaborationStorageKind,
    GeneratedCollaborationValueCodec, GeneratedMaterializationPlan, GeneratedMutationSpec,
    GeneratedProjectionSpec,
};
use clerkenwell_session::transport::{
    ProjectionErrorCode, ProjectionErrorEnvelope, ProjectionMutationCommand,
    ProjectionResyncCommand, ProjectionSubscribeAccepted, ProjectionSubscribeCommand,
    ProjectionTransportEvent,
};
use clerkenwell_session::{
    deliver_change, serve, AuthoringHeld, AuthoringState, AuthoringStates, Held, ProjectionCommand,
    ProjectionFailure, ProjectionHost, ProjectionRefusal, ProjectionRegistry,
    ProjectionSubscription, ProjectionSubscriptions,
};
use clerkenwell_store::testing::MemoryStorage;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationImportRequest,
    CollaborationService, CollaborationStores, CommitPolicy, ImportFence, PeerKey, StoreResult,
};
use serde_json::{json, Value};

const fn field(
    path: &'static str,
    storage_kind: GeneratedCollaborationStorageKind,
    container: Option<&'static str>,
    key: Option<&'static str>,
    conflict: GeneratedCollaborationConflict,
) -> GeneratedCollaborationFieldSpec {
    GeneratedCollaborationFieldSpec {
        path,
        storage_kind,
        container,
        container_template: None,
        key,
        identity_path: None,
        identity_variable: None,
        order_container: None,
        item_container_template: None,
        metadata_container: None,
        metadata_container_template: None,
        metadata_key: None,
        codec: GeneratedCollaborationValueCodec::String,
        value_schema: None,
        required: true,
        required_in_parent: true,
        conflict,
        writers: &[],
    }
}

/// The fields of a document named by `id` whose body concurrent edits merge.
macro_rules! fields {
    ($id:literal, $container:literal) => {
        &[
            field(
                $id,
                GeneratedCollaborationStorageKind::Scalar,
                Some($container),
                Some($id),
                GeneratedCollaborationConflict::Immutable,
            ),
            field(
                "body",
                GeneratedCollaborationStorageKind::Text,
                Some("body"),
                None,
                GeneratedCollaborationConflict::Merge,
            ),
            field(
                "etag",
                GeneratedCollaborationStorageKind::DerivedRevision,
                None,
                None,
                GeneratedCollaborationConflict::Immutable,
            ),
        ]
    };
}

/// Notes, each named by its id.
const NOTE_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Note",
    id_field: "note_id",
    schema_version: 1,
    authoring_projection: "notes.authoringState",
    authoring_document: None,
    authoring_store_params: &[],
    import_mutation: "note.importUpdate",
    root_container: "note",
    fields: fields!("note_id", "note"),
};
/// The one catalogue.
const CATALOGUE_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Catalogue",
    id_field: "catalogue_id",
    schema_version: 1,
    authoring_projection: "catalogue.authoringState",
    authoring_document: Some("catalogue"),
    authoring_store_params: &[],
    import_mutation: "catalogue.importUpdate",
    root_container: "catalogue",
    fields: fields!("catalogue_id", "catalogue"),
};
/// Drafts, kept in the store of the session they belong to.
const DRAFT_PLAN: GeneratedCollaborationEntitySpec = GeneratedCollaborationEntitySpec {
    name: "Draft",
    id_field: "draft_id",
    schema_version: 1,
    authoring_projection: "drafts.authoringState",
    authoring_document: None,
    authoring_store_params: &["session_id"],
    import_mutation: "draft.importUpdate",
    root_container: "draft",
    fields: fields!("draft_id", "draft"),
};
static PLANS: &[GeneratedCollaborationEntitySpec] = &[NOTE_PLAN, CATALOGUE_PLAN, DRAFT_PLAN];
static NOTE: &GeneratedCollaborationEntitySpec = &PLANS[0];
static CATALOGUE: &GeneratedCollaborationEntitySpec = &PLANS[1];
static DRAFT: &GeneratedCollaborationEntitySpec = &PLANS[2];

static PROJECTIONS: &[GeneratedProjectionSpec] = &[
    GeneratedProjectionSpec {
        name: "notes.authoringState",
        depends_on: &["Note"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
    GeneratedProjectionSpec {
        name: "catalogue.authoringState",
        depends_on: &["Catalogue"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
    GeneratedProjectionSpec {
        name: "drafts.authoringState",
        depends_on: &["Draft"],
        materialization: GeneratedMaterializationPlan::Reset,
    },
];
static MUTATIONS: &[GeneratedMutationSpec] = &[GeneratedMutationSpec {
    name: "note.touch",
    touches: &["Note"],
}];
static REGISTRY: ProjectionRegistry = ProjectionRegistry::new(PROJECTIONS, MUTATIONS, &[]);

const POLICY: CommitPolicy = CommitPolicy {
    retained_operations: 8,
    attempts: match NonZeroU32::new(8) {
        Some(attempts) => attempts,
        None => panic!("attempts are non-zero"),
    },
    backoff: Duration::from_millis(2),
};

const WORKSPACE: &str = "workspace";

fn accept(_: &Value) -> StoreResult<()> {
    Ok(())
}

#[derive(Debug)]
struct Failure(ProjectionRefusal);

impl ProjectionFailure for Failure {
    fn refused(refusal: ProjectionRefusal) -> Self {
        Self(refusal)
    }

    fn describe(&self) -> ProjectionRefusal {
        self.0.clone()
    }

    fn described(self, _envelope: ProjectionErrorEnvelope) -> Self {
        self
    }
}

type Subscriptions = Mutex<ProjectionSubscriptions<Value, Held<()>>>;

/// The principal the tests' clients connect as.
fn client() -> Principal {
    Principal::new("client", ActorKind::Human)
}

fn subscriptions_of_a_client() -> Subscriptions {
    Mutex::new(ProjectionSubscriptions::new(client()))
}

/// An application whose only projections are authoring states: notes and the
/// catalogue in its workspace store, and drafts in a store per session.
struct Library {
    authoring: AuthoringStates,
    changes: Arc<Mutex<Vec<ChangeEvent>>>,
}

impl Library {
    fn new(sessions: &[&str]) -> Self {
        let stores = CollaborationStores::new(
            PLANS,
            POLICY,
            PeerKey::new([7; 32]).expect("a key of 32 bytes"),
        );
        let changes = Arc::new(Mutex::new(Vec::new()));
        let heard = changes.clone();
        stores
            .changes()
            .listen(move |change| heard.lock().unwrap().push(change.clone()));
        for name in std::iter::once(WORKSPACE.to_owned())
            .chain(sessions.iter().map(|session| format!("session/{session}")))
        {
            stores
                .register(&name, MemoryStorage::default())
                .expect("register");
        }
        Self {
            authoring: AuthoringStates::new(stores, PLANS, |entity, params| match entity {
                "Draft" => Some(format!("session/{}", params["session_id"].as_str()?)),
                _ => Some(WORKSPACE.to_owned()),
            }),
            changes,
        }
    }

    fn store(&self, name: &str) -> CollaborationService {
        self.authoring.stores().store(name).expect("the store")
    }

    /// Writes the first state of `plan`'s document `id` in `store`.
    fn create(&self, store: &str, plan: &'static GeneratedCollaborationEntitySpec, id: &str) {
        self.store(store)
            .bootstrap(
                plan,
                &CollaborationDocumentId::new(plan.name, id),
                &json!({ plan.id_field: id, "body": "First.", "etag": "" }),
                accept,
            )
            .expect("bootstrap");
    }

    /// Commits a new body for `plan`'s document `id` in `store`, and returns
    /// the change the commit announced.
    fn commit(
        &self,
        store: &str,
        plan: &'static GeneratedCollaborationEntitySpec,
        id: &str,
        body: &str,
    ) -> ChangeEvent {
        let service = self.store(store);
        let document = CollaborationDocumentId::new(plan.name, id);
        let state = service
            .authoring_state(plan, &document, None)
            .expect("the accepted state");
        let mut replica = CollaborationReplica::from_versioned_update_base64(
            plan,
            plan.schema_version,
            &state.update_base64,
        )
        .expect("a replica");
        let block = service.allocate_peers(&client(), &document);
        replica.set_peer(block.base).expect("an allocated peer");
        let mut edited = replica.materialized_document("writer").expect("a document");
        edited["body"] = json!(body);
        replica.replace_document(&edited).expect("an edit");
        service
            .import(
                plan,
                CollaborationImportRequest {
                    document,
                    schema_version: plan.schema_version,
                    operation_id: format!("{id}-{body}"),
                    exchange_mode: CollaborationExchangeMode::Incremental,
                    base_frontier_base64: state.accepted_frontier_base64.clone(),
                    update_base64: replica
                        .export_incremental_update_base64(&state.accepted_frontier_base64)
                        .expect("an update"),
                    fence: ImportFence::Frontier,
                    actor: client(),
                    peer_nonces: vec![block.nonce],
                    intent: None,
                },
                accept,
            )
            .expect("import");
        self.last_change()
    }

    fn last_change(&self) -> ChangeEvent {
        self.changes
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("an announced change")
    }
}

impl ProjectionHost for Library {
    type Snapshot = Value;
    type Patch = Value;
    type Delivery = ();
    type MutationResult = Value;
    type Failure = Failure;

    fn registry(&self) -> &ProjectionRegistry {
        &REGISTRY
    }

    fn authoring_states(&self) -> Option<&AuthoringStates> {
        Some(&self.authoring)
    }

    async fn snapshot(
        &self,
        projection: &str,
        _params: &Value,
        _held: Option<&()>,
    ) -> Result<Value, Failure> {
        panic!("the session asked the host for {projection}")
    }

    fn delivered(&self, _snapshot: &Value) -> Option<()> {
        None
    }

    fn delivered_by_patch(&self, _patch: &Value) -> Option<()> {
        None
    }

    async fn mutate(
        &self,
        _principal: &Principal,
        _mutation: &str,
        _params: Value,
    ) -> Result<Value, Failure> {
        Ok(json!({}))
    }

    async fn mutation_patch(
        &self,
        _mutation: &str,
        _result: &Value,
        subscription: &ProjectionSubscription<Held<()>>,
    ) -> Option<Value> {
        panic!(
            "the session asked the host to patch {}",
            subscription.projection
        )
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

fn subscribe(
    library: &Library,
    connection: &Subscriptions,
    projection: &str,
    params: Value,
    held: Option<Value>,
) -> Result<
    (
        ProjectionSubscribeAccepted,
        Vec<ProjectionTransportEvent<Value, Value>>,
    ),
    Failure,
> {
    let reply = block_on(serve(
        library,
        connection,
        ProjectionCommand::Subscribe(ProjectionSubscribeCommand {
            projection: projection.to_owned(),
            params: Some(params),
            held,
        }),
    ))?;
    Ok((
        serde_json::from_value(reply.result).expect("an accepted subscription"),
        reply.events,
    ))
}

/// The authoring state a snapshot delivers, and the token it leaves held.
fn snapshot_state(event: &ProjectionTransportEvent<Value, Value>) -> (AuthoringState, Value) {
    let ProjectionTransportEvent::Snapshot { snapshot, held, .. } = event else {
        panic!("a snapshot: {event:?}");
    };
    (
        serde_json::from_value(snapshot["value"].clone()).expect("an authoring state"),
        held.clone().expect("a snapshot names what it leaves held"),
    )
}

/// The patch an authoring-state delivery sends.
fn patch_value(event: &ProjectionTransportEvent<Value, Value>) -> &Value {
    let ProjectionTransportEvent::Patch { patch, .. } = event else {
        panic!("a patch: {event:?}");
    };
    &patch["value"]
}

/// A replica built from `state`, as a client that held nothing builds it.
fn replica(
    plan: &'static GeneratedCollaborationEntitySpec,
    state: &AuthoringState,
) -> CollaborationReplica {
    CollaborationReplica::from_versioned_update_base64(
        plan,
        state.schema_version,
        &state.update_base64,
    )
    .expect("a replica")
}

#[test]
fn an_authoring_state_allocates_its_subscriber_a_block_of_peers_to_write_under() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let connection = subscriptions_of_a_client();
    let (_, events) = subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");
    let (state, _) = snapshot_state(&events[0]);
    let block = &state.peer_block;
    let base: u64 = block.base.parse().expect("a decimal peer");
    assert_eq!(base % (1 << block.index_bits), 0, "the block's first peer");

    let mut writer = replica(NOTE, &state);
    writer
        .set_peer(base | 3)
        .expect("a peer of the allocated block");
    let mut edited = writer.materialized_document("writer").expect("a document");
    edited["body"] = json!("Written under the allocated block.");
    writer.replace_document(&edited).expect("an edit");
    let document = CollaborationDocumentId::new("Note", "note-1");
    let import = |actor: Principal| {
        library.store(WORKSPACE).import(
            NOTE,
            CollaborationImportRequest {
                document: document.clone(),
                schema_version: NOTE.schema_version,
                operation_id: "allocated".to_owned(),
                exchange_mode: CollaborationExchangeMode::Incremental,
                base_frontier_base64: state.accepted_frontier_base64.clone(),
                update_base64: writer
                    .export_incremental_update_base64(&state.accepted_frontier_base64)
                    .expect("an update"),
                fence: ImportFence::Frontier,
                actor,
                peer_nonces: vec![block.nonce.clone()],
                intent: None,
            },
            accept,
        )
    };

    import(Principal::new("someone else", ActorKind::Human))
        .expect_err("the block is the subscriber's");
    import(client()).expect("the subscriber writes under its block");
    assert_eq!(library.last_change().actorid, client().id);
}

fn body(replica: &CollaborationReplica) -> Value {
    replica.materialized_document("reader").expect("a document")["body"].clone()
}

fn note(id: &str) -> Value {
    json!({ "note_id": id })
}

#[test]
fn a_subscription_takes_the_accepted_state_of_the_document_it_names() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let connection = subscriptions_of_a_client();

    let (_, events) = subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");

    let (state, held) = snapshot_state(&events[0]);
    let accepted = library
        .store(WORKSPACE)
        .authoring_state(NOTE, &CollaborationDocumentId::new("Note", "note-1"), None)
        .expect("the accepted state");
    assert_eq!(
        state.accepted_frontier_base64,
        accepted.accepted_frontier_base64
    );
    assert_eq!(state.etag, accepted.etag);
    assert_eq!(state.schema_version, NOTE.schema_version);
    assert_eq!(
        serde_json::from_value::<AuthoringHeld>(held).expect("an authoring token"),
        AuthoringHeld::from(&state)
    );
}

#[test]
fn a_client_that_holds_the_accepted_state_is_sent_nothing_when_it_subscribes_again() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let (_, first) = subscribe(
        &library,
        &subscriptions_of_a_client(),
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");
    let (_, held) = snapshot_state(&first[0]);

    let (accepted, events) = subscribe(
        &library,
        &subscriptions_of_a_client(),
        "notes.authoringState",
        note("note-1"),
        Some(held),
    )
    .expect("subscribe again");

    assert!(accepted.up_to_date);
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn a_client_holding_a_state_of_a_reseeded_document_is_told_to_replace_it() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let (_, first) = subscribe(
        &library,
        &subscriptions_of_a_client(),
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");
    let (state, held) = snapshot_state(&first[0]);
    assert!(
        !state.replaces_held,
        "a client that held nothing replaces nothing"
    );
    library
        .store(WORKSPACE)
        .delete(&client(), &CollaborationDocumentId::new("Note", "note-1"))
        .expect("delete");
    library.create(WORKSPACE, NOTE, "note-1");

    let (accepted, events) = subscribe(
        &library,
        &subscriptions_of_a_client(),
        "notes.authoringState",
        note("note-1"),
        Some(held),
    )
    .expect("subscribe again");

    assert!(!accepted.up_to_date);
    let (state, _) = snapshot_state(&events[0]);
    assert!(
        state.replaces_held,
        "the client's state is of another history"
    );
}

#[test]
fn a_commit_sends_each_subscription_following_the_document_the_operations_its_client_lacks() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    library.create(WORKSPACE, NOTE, "note-2");
    let connection = subscriptions_of_a_client();
    let (first, events) = subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");
    let (snapshot, _) = snapshot_state(&events[0]);
    subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-2"),
        None,
    )
    .expect("subscribe");

    let change = library.commit(WORKSPACE, NOTE, "note-1", "Second.");
    let events = block_on(deliver_change(&library, &connection, &change));

    assert_eq!(events.len(), 1, "{events:?}");
    let ProjectionTransportEvent::Patch {
        subscription_id,
        from_revision,
        ..
    } = &events[0]
    else {
        panic!("a patch: {events:?}");
    };
    assert_eq!(*subscription_id, first.subscription_id);
    assert_eq!(*from_revision, first.revision);
    let patch = patch_value(&events[0]);
    assert_eq!(patch["kind"], "replace");
    let state: AuthoringState =
        serde_json::from_value(patch["state"].clone()).expect("an authoring state");
    let mut held = replica(NOTE, &snapshot);
    held.adopt_versioned_update_base64(state.schema_version, &state.update_base64)
        .expect("the operations the client lacks");
    assert_eq!(body(&held), "Second.");
    assert_eq!(
        Some(state.accepted_frontier_base64),
        change.data.frontier_after
    );
}

#[test]
fn a_change_a_subscription_already_holds_is_not_sent_again() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let connection = subscriptions_of_a_client();
    subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");
    let change = library.commit(WORKSPACE, NOTE, "note-1", "Second.");
    assert_eq!(
        block_on(deliver_change(&library, &connection, &change)).len(),
        1
    );

    let again = block_on(deliver_change(&library, &connection, &change));

    assert!(again.is_empty(), "{again:?}");
}

#[test]
fn a_deleted_document_is_removed_from_every_subscription_following_it() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let connection = subscriptions_of_a_client();
    let (accepted, _) = subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");

    library
        .store(WORKSPACE)
        .delete(&client(), &CollaborationDocumentId::new("Note", "note-1"))
        .expect("delete");
    let events = block_on(deliver_change(
        &library,
        &connection,
        &library.last_change(),
    ));

    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(patch_value(&events[0])["kind"], "remove");
    assert!(connection
        .lock()
        .unwrap()
        .get(accepted.subscription_id)
        .expect("the subscription")
        .delivered
        .is_none());
    let again = block_on(deliver_change(
        &library,
        &connection,
        &library.last_change(),
    ));
    assert!(again.is_empty(), "{again:?}");
}

#[test]
fn a_change_reaches_only_subscriptions_to_the_store_that_announced_it() {
    let library = Library::new(&["one", "two"]);
    library.create("session/one", DRAFT, "draft-1");
    library.create("session/two", DRAFT, "draft-1");
    let connection = subscriptions_of_a_client();
    let (one, _) = subscribe(
        &library,
        &connection,
        "drafts.authoringState",
        json!({ "draft_id": "draft-1", "session_id": "one" }),
        None,
    )
    .expect("subscribe");
    subscribe(
        &library,
        &connection,
        "drafts.authoringState",
        json!({ "draft_id": "draft-1", "session_id": "two" }),
        None,
    )
    .expect("subscribe");

    let change = library.commit("session/one", DRAFT, "draft-1", "Second.");
    let events = block_on(deliver_change(&library, &connection, &change));

    assert_eq!(events.len(), 1, "{events:?}");
    let ProjectionTransportEvent::Patch {
        subscription_id, ..
    } = &events[0]
    else {
        panic!("a patch: {events:?}");
    };
    assert_eq!(*subscription_id, one.subscription_id);
}

#[test]
fn an_entity_with_one_document_is_followed_without_naming_it() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, CATALOGUE, "catalogue");
    let connection = subscriptions_of_a_client();
    subscribe(
        &library,
        &connection,
        "catalogue.authoringState",
        json!({}),
        None,
    )
    .expect("subscribe");

    let change = library.commit(WORKSPACE, CATALOGUE, "catalogue", "Second.");
    let events = block_on(deliver_change(&library, &connection, &change));

    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(patch_value(&events[0])["kind"], "replace");
}

#[test]
fn a_subscription_missing_a_parameter_that_chooses_the_store_is_refused() {
    let library = Library::new(&["one"]);
    library.create("session/one", DRAFT, "draft-1");

    let refused = subscribe(
        &library,
        &subscriptions_of_a_client(),
        "drafts.authoringState",
        json!({ "draft_id": "draft-1" }),
        None,
    )
    .expect_err("a refusal");

    assert_eq!(refused.0.code, ProjectionErrorCode::InvalidParams);
}

#[test]
fn a_document_no_store_keeps_is_not_found() {
    let library = Library::new(&[]);

    let refused = subscribe(
        &library,
        &subscriptions_of_a_client(),
        "drafts.authoringState",
        json!({ "draft_id": "draft-1", "session_id": "gone" }),
        None,
    )
    .expect_err("a refusal");

    assert_eq!(refused.0.code, ProjectionErrorCode::NotFound);
}

#[test]
fn a_mutation_sends_authoring_state_subscriptions_nothing() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let connection = subscriptions_of_a_client();
    subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");

    let reply = block_on(serve(
        &library,
        &connection,
        ProjectionCommand::Mutate(ProjectionMutationCommand {
            mutation: "note.touch".to_owned(),
            operation_id: None,
            base_revision: None,
            params: json!({}),
        }),
    ))
    .expect("mutate");

    assert!(reply.events.is_empty(), "{:?}", reply.events);
}

#[test]
fn a_resynchronised_subscription_takes_the_whole_accepted_state() {
    let library = Library::new(&[]);
    library.create(WORKSPACE, NOTE, "note-1");
    let connection = subscriptions_of_a_client();
    let (accepted, _) = subscribe(
        &library,
        &connection,
        "notes.authoringState",
        note("note-1"),
        None,
    )
    .expect("subscribe");
    library.commit(WORKSPACE, NOTE, "note-1", "Second.");

    let reply = block_on(serve(
        &library,
        &connection,
        ProjectionCommand::Resync(ProjectionResyncCommand {
            subscription_id: accepted.subscription_id,
        }),
    ))
    .expect("resync");

    let (state, _) = snapshot_state(&reply.events[0]);
    assert_eq!(body(&replica(NOTE, &state)), "Second.");
}

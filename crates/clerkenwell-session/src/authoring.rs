//! Authoring states: the projections a client keeps a replica of a
//! collaborative document in step with.
//!
//! Every collaborative entity's authoring state has the same snapshot and
//! patch. The session answers a subscription to one from the store the
//! application keeps the document in, and delivers each change a store
//! announces to the subscriptions that follow the document.

use std::fmt;
use std::sync::Arc;

use clerkenwell_events::Principal;
use clerkenwell_schema::GeneratedCollaborationEntitySpec;
use clerkenwell_store::{
    CollaborationDocumentId, CollaborationExchangeMode, CollaborationStores, StoreError,
    StoreErrorKind, PEER_INDEX_BITS,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::server::ProjectionRefusal;
use crate::transport::ProjectionErrorCode;

/// A document's accepted state as an authoring-state update delivers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthoringState {
    /// The layout version the document's operations are written in.
    pub schema_version: u32,
    /// The frontier of the accepted state the update leaves its client holding.
    pub accepted_frontier_base64: String,
    /// The accepted operations the update adds: every one in a snapshot, and
    /// in a patch those after the frontier its client held.
    pub update_base64: String,
    pub etag: String,
    /// How the store accepts an import of the document.
    pub exchange_modes: Vec<CollaborationExchangeMode>,
    /// A new block of peers for the client to write the document under.
    pub peer_block: AuthoringPeerBlock,
}

/// A block of peers allocated to the subscriber: every peer whose bits above
/// the low `index_bits` are `base`'s. An import names the block by its nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthoringPeerBlock {
    pub nonce: String,
    /// The block's first peer, in decimal.
    pub base: String,
    pub index_bits: u32,
}

/// What an authoring-state update leaves its client holding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthoringHeld {
    pub schema_version: u32,
    pub frontier: String,
    pub etag: String,
}

impl From<&AuthoringState> for AuthoringHeld {
    fn from(state: &AuthoringState) -> Self {
        Self {
            schema_version: state.schema_version,
            frontier: state.accepted_frontier_base64.clone(),
            etag: state.etag.clone(),
        }
    }
}

/// What an update leaves its client holding: an authoring state's accepted
/// state, or what the host names for one of its own projections. A client
/// sends it back when it subscribes again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Held<D> {
    Authoring(AuthoringHeld),
    Host(D),
}

impl<D> Held<D> {
    /// The accepted state held, for an authoring state.
    pub fn authoring(&self) -> Option<&AuthoringHeld> {
        match self {
            Self::Authoring(held) => Some(held),
            Self::Host(_) => None,
        }
    }
}

/// The name of the store in its set that keeps an `entity` document, from the
/// parameters the entity declares as choosing it; `None` when none keeps it.
type StoreRoute = dyn Fn(&str, &Value) -> Option<String> + Send + Sync;

/// The authoring states of an application's collaborative entities, read
/// from the stores it keeps their documents in.
#[derive(Clone)]
pub struct AuthoringStates {
    stores: CollaborationStores,
    plans: &'static [GeneratedCollaborationEntitySpec],
    route: Arc<StoreRoute>,
}

/// The document an authoring-state subscription follows, and the store that
/// keeps it.
pub(crate) struct FollowedDocument {
    pub plan: &'static GeneratedCollaborationEntitySpec,
    pub id: CollaborationDocumentId,
    pub store: String,
}

impl AuthoringStates {
    /// The authoring states of `plans`' entities, whose documents live in
    /// `stores`. `route` names the store that keeps an entity's document from
    /// the parameters the entity's `storeParams` declares.
    pub fn new(
        stores: CollaborationStores,
        plans: &'static [GeneratedCollaborationEntitySpec],
        route: impl Fn(&str, &Value) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            stores,
            plans,
            route: Arc::new(route),
        }
    }

    pub fn stores(&self) -> &CollaborationStores {
        &self.stores
    }

    /// The entity whose authoring state `projection` is.
    pub fn plan(&self, projection: &str) -> Option<&'static GeneratedCollaborationEntitySpec> {
        self.plans
            .iter()
            .find(|plan| plan.authoring_projection == projection)
    }

    /// The document a subscription to `projection` with `params` follows,
    /// when `projection` is an authoring state.
    pub(crate) fn follow(
        &self,
        projection: &str,
        params: &Value,
    ) -> Option<Result<FollowedDocument, ProjectionRefusal>> {
        let plan = self.plan(projection)?;
        Some(followed(plan, params).and_then(|(id, store_params)| {
            let store = (self.route)(plan.name, &store_params).ok_or_else(|| {
                ProjectionRefusal::new(
                    ProjectionErrorCode::NotFound,
                    format!("No store keeps {}/{}.", id.entity, id.resource_id),
                    Some(json!({ "entity": id.entity, "resource_id": id.resource_id })),
                )
            })?;
            Ok(FollowedDocument { plan, id, store })
        }))
    }

    /// The state of `document` for a client that holds `held`: only the
    /// operations it lacks, and a new block of peers for `principal`.
    pub(crate) fn state(
        &self,
        document: &FollowedDocument,
        held: Option<&AuthoringHeld>,
        principal: &Principal,
    ) -> Result<AuthoringState, ProjectionRefusal> {
        let service = self.stores.store(&document.store).ok_or_else(|| {
            ProjectionRefusal::new(
                ProjectionErrorCode::NotFound,
                format!(
                    "No store {:?} keeps {}/{}.",
                    document.store, document.id.entity, document.id.resource_id
                ),
                Some(json!({ "store": document.store, "entity": document.id.entity })),
            )
        })?;
        let read = service
            .authoring_state(
                document.plan,
                &document.id,
                held.map(|held| held.frontier.as_str()),
            )
            .map_err(ProjectionRefusal::from)?;
        let block = service.allocate_peers(principal, &document.id);
        Ok(AuthoringState {
            schema_version: read.schema_version,
            accepted_frontier_base64: read.accepted_frontier_base64,
            update_base64: read.update_base64,
            etag: read.etag,
            exchange_modes: vec![
                CollaborationExchangeMode::Incremental,
                CollaborationExchangeMode::Bootstrap,
            ],
            peer_block: AuthoringPeerBlock {
                nonce: block.nonce,
                base: block.base.to_string(),
                index_bits: PEER_INDEX_BITS,
            },
        })
    }
}

impl fmt::Debug for AuthoringStates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthoringStates")
            .field("stores", &self.stores)
            .field("plans", &self.plans)
            .finish_non_exhaustive()
    }
}

/// The document a subscription to `plan`'s authoring state with `params`
/// follows, and the parameters that choose the store it lives in.
fn followed(
    plan: &GeneratedCollaborationEntitySpec,
    params: &Value,
) -> Result<(CollaborationDocumentId, Value), ProjectionRefusal> {
    let param = |name: &str| {
        params.get(name).and_then(Value::as_str).ok_or_else(|| {
            ProjectionRefusal::new(
                ProjectionErrorCode::InvalidParams,
                format!(
                    "Subscribing to {} names its document with {name:?}.",
                    plan.authoring_projection
                ),
                Some(json!({ "projection": plan.authoring_projection, "param": name })),
            )
        })
    };
    let resource_id = match plan.authoring_document {
        Some(document) => document,
        None => param(plan.id_field)?,
    };
    let mut store_params = Map::new();
    for name in plan.authoring_store_params {
        store_params.insert((*name).to_owned(), Value::from(param(name)?));
    }
    Ok((
        CollaborationDocumentId::new(plan.name, resource_id),
        Value::Object(store_params),
    ))
}

impl From<StoreError> for ProjectionRefusal {
    /// A store's refusal as the protocol reports it: the code its data names,
    /// or the one its kind implies.
    fn from(error: StoreError) -> Self {
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
        ProjectionRefusal::new(code, error.message, error.data)
    }
}

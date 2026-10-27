//! Reading and editing an application's collaborative documents.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use clapham::{Call, Category, Contribution, Effect, Locality, Refusal, TypedOperation};
use clerkenwell_schema::GeneratedCollaborationEntitySpec;
use clerkenwell_session::AuthoringStates;
use clerkenwell_store::{
    CollaborationDocumentEdit, CollaborationDocumentId, CollaborationDocumentRead,
    CollaborationDocumentSummary, CollaborationImportResult, CollaborationService,
    CollaborationTextEdit, CollaborationVersion, StoreError,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::host::{Invalid, OperationsHost};
use crate::policies::DocumentScope;
use crate::refusals::{self, CALLER_REFUSALS, STORE_REFUSALS, VALIDATION_FAILED};
use crate::{principal, DOCUMENTS_READ, DOCUMENTS_WRITE};

/// The documents of one entity.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListInput {
    #[schemars(length(min = 1))]
    pub entity: String,
    /// The parameters that choose the store keeping the entity's documents,
    /// where it declares any.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub store: BTreeMap<String, String>,
}

/// One document.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetInput {
    #[schemars(length(min = 1))]
    pub entity: String,
    #[schemars(length(min = 1))]
    pub id: String,
    /// The parameters that choose the store keeping the document, where its
    /// entity declares any.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub store: BTreeMap<String, String>,
}

/// A whole document, edited from the one read at `etag`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditInput {
    #[schemars(length(min = 1))]
    pub entity: String,
    #[schemars(length(min = 1))]
    pub id: String,
    /// The parameters that choose the store keeping the document, where its
    /// entity declares any.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub store: BTreeMap<String, String>,
    /// Names the edit: a retry under it is answered with the result the edit
    /// was accepted with.
    #[schemars(length(min = 1))]
    pub operation_id: String,
    /// The etag of the document as it was read.
    #[schemars(length(min = 1))]
    pub etag: String,
    /// The document as it was read, edited.
    pub document: Value,
    /// Why the change is made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
}

/// A replacement of the text one text field held at a frontier.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditTextInput {
    #[schemars(length(min = 1))]
    pub entity: String,
    #[schemars(length(min = 1))]
    pub id: String,
    /// The parameters that choose the store keeping the document, where its
    /// entity declares any.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub store: BTreeMap<String, String>,
    /// Names the edit: a retry under it is answered with the result the edit
    /// was accepted with.
    #[schemars(length(min = 1))]
    pub operation_id: String,
    /// The text field's path in the entity's collaboration plan.
    #[schemars(length(min = 1))]
    pub field_path: String,
    /// The identity of each keyed item on the field's path, by its variable.
    #[serde(default)]
    pub identities: BTreeMap<String, String>,
    /// The frontier of the document as it was read.
    #[schemars(length(min = 1))]
    pub frontier_base64: String,
    /// The text the field held at the frontier.
    pub read: String,
    pub replacement: String,
    /// Why the change is made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
}

impl ListInput {
    pub(crate) fn scope(&self) -> DocumentScope {
        DocumentScope::new(&self.entity, None, &self.store)
    }
}

impl GetInput {
    pub(crate) fn scope(&self) -> DocumentScope {
        DocumentScope::new(&self.entity, Some(&self.id), &self.store)
    }
}

impl EditInput {
    pub(crate) fn scope(&self) -> DocumentScope {
        DocumentScope::new(&self.entity, Some(&self.id), &self.store)
    }
}

impl EditTextInput {
    pub(crate) fn scope(&self) -> DocumentScope {
        DocumentScope::new(&self.entity, Some(&self.id), &self.store)
    }
}

/// A document's version: the history it belongs to, and how far each of the
/// peers that wrote it has written.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentVersion {
    pub incarnation: String,
    /// By peer, in decimal.
    pub peers: BTreeMap<String, u64>,
}

impl From<CollaborationVersion> for DocumentVersion {
    fn from(version: CollaborationVersion) -> Self {
        Self {
            incarnation: version.incarnation,
            peers: version
                .peers
                .into_iter()
                .map(|(peer, counter)| (peer.to_string(), counter))
                .collect(),
        }
    }
}

/// What a stored document is.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentSummary {
    pub id: String,
    pub etag: String,
    pub generation: u64,
    pub version: DocumentVersion,
}

impl From<CollaborationDocumentSummary> for DocumentSummary {
    fn from(summary: CollaborationDocumentSummary) -> Self {
        Self {
            id: summary.document.resource_id,
            etag: summary.etag,
            generation: summary.generation,
            version: summary.version.into(),
        }
    }
}

/// The documents of one entity, in id order.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Documents {
    pub documents: Vec<DocumentSummary>,
}

/// A document as one read found it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    pub document: Value,
    pub etag: String,
    pub generation: u64,
    pub version: DocumentVersion,
    /// The frontier a text edit names as the document it read.
    pub accepted_frontier_base64: String,
}

impl From<CollaborationDocumentRead> for Document {
    fn from(read: CollaborationDocumentRead) -> Self {
        Self {
            document: read.document,
            etag: read.summary.etag,
            generation: read.summary.generation,
            version: read.summary.version.into(),
            accepted_frontier_base64: read.accepted_frontier_base64,
        }
    }
}

/// The document an edit left accepted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Edited {
    pub document: Value,
    pub etag: String,
    pub generation: u64,
    pub version: DocumentVersion,
    /// The frontier a text edit names as the document it read.
    pub accepted_frontier_base64: String,
    /// Whether the call was a retry of an edit already accepted.
    pub duplicate: bool,
}

impl From<CollaborationImportResult> for Edited {
    fn from(imported: CollaborationImportResult) -> Self {
        Self {
            document: imported.materialized,
            etag: imported.etag,
            generation: imported.generation,
            version: imported.version.into(),
            accepted_frontier_base64: imported.accepted_frontier_base64,
            duplicate: imported.duplicate,
        }
    }
}

pub(crate) const LIST: &str = "clerkenwell.document.list";
pub(crate) const GET: &str = "clerkenwell.document.get";
pub(crate) const EDIT: &str = "clerkenwell.document.edit";
pub(crate) const EDIT_TEXT: &str = "clerkenwell.document.editText";

/// Why an edit was not committed: the store refused or failed it, or the
/// application refused the document it would leave.
enum EditFailure {
    Store(StoreError),
    Invalid(Invalid),
}

impl From<StoreError> for EditFailure {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<EditFailure> for Refusal {
    fn from(failure: EditFailure) -> Self {
        match failure {
            EditFailure::Store(error) => refusals::store(error),
            EditFailure::Invalid(invalid) => refusals::invalid(invalid),
        }
    }
}

/// `operation`, declaring the refusals of a document operation.
fn declared<I: JsonSchema, O: JsonSchema>(
    name: &str,
    description: &str,
    effect: Effect,
    scope: &str,
) -> TypedOperation<I, O> {
    let mut operation = TypedOperation::new(name, description, effect)
        .expect("Clerkenwell's operation names are valid")
        .locality(Locality::Daemon)
        .require_scope(scope)
        // Each document operation names a policy of its own name, which
        // `document_policies` makes.
        .policy(name)
        .idempotent(true)
        .open_world(false);
    for (code, category) in STORE_REFUSALS.iter().chain(CALLER_REFUSALS) {
        operation = operation.refusal(*code, *category);
    }
    operation
}

/// The document operations over `authoring`'s stores, for `host`'s
/// principals.
pub(crate) fn contribute(
    contribution: Contribution,
    authoring: AuthoringStates,
    host: Arc<dyn OperationsHost>,
) -> Contribution {
    let authoring = Arc::new(authoring);
    let (list_authoring, list_host) = (authoring.clone(), host.clone());
    let (get_authoring, get_host) = (authoring.clone(), host.clone());
    let (edit_authoring, edit_host) = (authoring.clone(), host.clone());
    let (text_authoring, text_host) = (authoring, host);
    contribution
        .typed_operation(
            declared::<ListInput, Documents>(
                LIST,
                "The documents of one collaborative entity, each with its etag, generation and version.",
                Effect::Query,
                DOCUMENTS_READ,
            ),
            move |call: Call, input: ListInput| {
                let (authoring, host) = (list_authoring.clone(), list_host.clone());
                async move {
                    principal(&*host, &call)?;
                    let (plan, service) = store(&authoring, &input.entity, &input.store)?;
                    let summaries = service.summaries(plan.name).map_err(refusals::store)?;
                    Ok(Documents {
                        documents: summaries.into_iter().map(DocumentSummary::from).collect(),
                    })
                }
            },
        )
        .typed_operation(
            declared::<GetInput, Document>(
                GET,
                "A collaborative document as JSON, with its etag, generation, version and the frontier a text edit names.",
                Effect::Query,
                DOCUMENTS_READ,
            ),
            move |call: Call, input: GetInput| {
                let (authoring, host) = (get_authoring.clone(), get_host.clone());
                async move {
                    principal(&*host, &call)?;
                    let (plan, service, document) = located(&authoring, &input.entity, &input.id, &input.store)?;
                    let read = service
                        .read_document(plan, &document)
                        .map_err(refusals::store)?
                        .ok_or_else(|| absent(&document))?;
                    Ok(Document::from(read))
                }
            },
        )
        .typed_operation(
            declared::<EditInput, Edited>(
                EDIT,
                "Edits a whole collaborative document: takes the document as it was read, edited, and the etag it was read at. A document changed since is refused with the accepted document and its etag.",
                Effect::Mutation,
                DOCUMENTS_WRITE,
            )
            .refusal(VALIDATION_FAILED.0, VALIDATION_FAILED.1),
            move |call: Call, input: EditInput| {
                let (authoring, host) = (edit_authoring.clone(), edit_host.clone());
                async move {
                    let actor = principal(&*host, &call)?;
                    let (plan, service, document) = located(&authoring, &input.entity, &input.id, &input.store)?;
                    let edited = service.edit(
                        plan,
                        CollaborationDocumentEdit {
                            document,
                            operation_id: input.operation_id,
                            edited: input.document,
                            etag: input.etag,
                            actor,
                            intent: input.intent,
                        },
                        |document| host.validate(plan.name, document).map_err(EditFailure::Invalid),
                    )?;
                    Ok(Edited::from(edited))
                }
            },
        )
        .typed_operation(
            declared::<EditTextInput, Edited>(
                EDIT_TEXT,
                "Replaces the text one text field of a collaborative document held at the frontier it was read at. Edits made since merge with it.",
                Effect::Mutation,
                DOCUMENTS_WRITE,
            )
            .refusal(VALIDATION_FAILED.0, VALIDATION_FAILED.1),
            move |call: Call, input: EditTextInput| {
                let (authoring, host) = (text_authoring.clone(), text_host.clone());
                async move {
                    let actor = principal(&*host, &call)?;
                    let (plan, service, document) = located(&authoring, &input.entity, &input.id, &input.store)?;
                    let edited = service.edit_text(
                        plan,
                        CollaborationTextEdit {
                            document,
                            operation_id: input.operation_id,
                            field_path: input.field_path,
                            identities: input.identities.into_iter().collect::<HashMap<_, _>>(),
                            frontier_base64: input.frontier_base64,
                            read: input.read,
                            replacement: input.replacement,
                            actor,
                            intent: input.intent,
                        },
                        |document| host.validate(plan.name, document).map_err(EditFailure::Invalid),
                    )?;
                    Ok(Edited::from(edited))
                }
            },
        )
}

/// The plan of `entity` and the store that keeps its documents, chosen by
/// `store`, the store parameters the entity declares.
fn store(
    authoring: &AuthoringStates,
    entity: &str,
    store: &BTreeMap<String, String>,
) -> Result<
    (
        &'static GeneratedCollaborationEntitySpec,
        CollaborationService,
    ),
    Refusal,
> {
    let plan = authoring.entity(entity).ok_or_else(|| {
        refusals::not_found(
            format!("No collaborative entity is named \"{entity}\"."),
            json!({ "entity": entity }),
        )
    })?;
    let declared: BTreeSet<&str> = plan.authoring_store_params.iter().copied().collect();
    if store.keys().map(String::as_str).collect::<BTreeSet<_>>() != declared {
        return Err(refusals::invalid_params(
            format!("{entity}'s store is chosen by {declared:?}."),
            json!({ "entity": entity, "store_params": declared }),
        ));
    }
    let params: Map<String, Value> = store
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    let service = authoring
        .route(entity, &Value::Object(params))
        .and_then(|name| authoring.stores().store(&name))
        .ok_or_else(|| {
            refusals::not_found(
                format!("No store keeps {entity} documents with these store parameters."),
                json!({ "entity": entity }),
            )
        })?;
    Ok((plan, service))
}

/// The plan of `entity`, the store that keeps its document `id`, and the
/// document.
fn located(
    authoring: &AuthoringStates,
    entity: &str,
    id: &str,
    store_params: &BTreeMap<String, String>,
) -> Result<
    (
        &'static GeneratedCollaborationEntitySpec,
        CollaborationService,
        CollaborationDocumentId,
    ),
    Refusal,
> {
    let (plan, service) = store(authoring, entity, store_params)?;
    let document = CollaborationDocumentId::new(plan.name, id);
    if plan.authoring_document.is_some_and(|only| only != id) {
        return Err(absent(&document));
    }
    Ok((plan, service, document))
}

/// The refusal of a document no store keeps.
fn absent(document: &CollaborationDocumentId) -> Refusal {
    let mut refusal = Refusal::new(
        "collaboration_document_not_found",
        Category::NotFound,
        format!(
            "No collaborative {} document exists for \"{}\".",
            document.entity, document.resource_id
        ),
    );
    refusal.details = Some(json!({
        "entity": document.entity,
        "resource_id": document.resource_id,
    }));
    refusal
}

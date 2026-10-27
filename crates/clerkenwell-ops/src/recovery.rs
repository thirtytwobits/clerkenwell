//! Recovering an application's stored documents, as an operator does.

use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clapham::{Call, Contribution, Effect, Locality, Refusal, TypedOperation};
use clerkenwell_store::{
    CollaborationAuditQuery, CollaborationAuditRecord, CollaborationDocumentId,
    CollaborationDocumentInspection, CollaborationService, CollaborationStores,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::host::OperationsHost;
use crate::refusals::{self, CALLER_REFUSALS, STORE_REFUSALS};
use crate::{principal, RECOVERY_ADMIN, RECOVERY_READ, RECOVERY_RESET};

/// No input.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Nothing {}

/// One store.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoreInput {
    #[schemars(length(min = 1))]
    pub store: String,
}

/// One document of a store.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredDocument {
    #[schemars(length(min = 1))]
    pub store: String,
    #[schemars(length(min = 1))]
    pub entity: String,
    #[schemars(length(min = 1))]
    pub id: String,
}

/// One document of a store, and why the operator acts on it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reasoned {
    #[schemars(length(min = 1))]
    pub store: String,
    #[schemars(length(min = 1))]
    pub entity: String,
    #[schemars(length(min = 1))]
    pub id: String,
    /// Recorded in the audit by its digest.
    #[schemars(length(min = 1))]
    pub reason: String,
}

/// The audit records of a store a query selects.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditInput {
    #[schemars(length(min = 1))]
    pub store: String,
    /// With `id`, the one document whose records are selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The principal whose requests are selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_unix_ms: Option<u64>,
}

/// The application's stores, by name.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stores {
    pub stores: Vec<String>,
}

/// What a document's stored data holds, and whether it can be read.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inspection {
    pub entity: String,
    pub id: String,
    pub schema_version: u32,
    pub generation: u64,
    pub checkpoint_sequence: u64,
    pub compacted_through_sequence: u64,
    pub retained_operation_count: usize,
    pub checkpoint_bytes: usize,
    pub retained_operation_bytes: usize,
    pub migration_required: bool,
    pub valid: bool,
    /// Why the data cannot be read, where it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

impl From<CollaborationDocumentInspection> for Inspection {
    fn from(inspection: CollaborationDocumentInspection) -> Self {
        Self {
            entity: inspection.entity,
            id: inspection.resource_id,
            schema_version: inspection.schema_version,
            generation: inspection.generation,
            checkpoint_sequence: inspection.checkpoint_sequence,
            compacted_through_sequence: inspection.compacted_through_sequence,
            retained_operation_count: inspection.retained_operation_count,
            checkpoint_bytes: inspection.checkpoint_bytes,
            retained_operation_bytes: inspection.retained_operation_bytes,
            migration_required: inspection.migration_required,
            valid: inspection.valid,
            failure_code: inspection.failure_code,
        }
    }
}

/// A document's stored envelope.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Exported {
    pub envelope_base64: String,
}

/// One recorded recovery request or refused import.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditRecord {
    pub timestamp_unix_ms: u64,
    pub actor: String,
    /// `human`, `agent`, `service` or `system`.
    pub actor_kind: String,
    pub entity: String,
    pub id: String,
    /// The request or refusal, as the store records it.
    pub event: Value,
}

/// A store's audit records, oldest first.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Audit {
    pub records: Vec<AuditRecord>,
}

/// Where the store kept the evidence of a recovery.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Evidence {
    pub evidence: String,
}

/// What a reindex read.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reindexed {
    /// The documents read.
    pub documents: usize,
    /// The fingerprint of the redacted catalogue.
    pub fingerprint: String,
}

/// No output.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Done {}

/// `name`, declaring the refusals of a recovery operation.
fn declared<I: JsonSchema, O: JsonSchema>(
    name: &str,
    description: &str,
    effect: Effect,
    scope: &str,
) -> TypedOperation<I, O> {
    let mut operation = TypedOperation::new(name, description, effect)
        .expect("Clerkenwell's operation names are valid")
        .locality(Locality::Workspace)
        .require_scope(scope)
        .open_world(false);
    for (code, category) in STORE_REFUSALS.iter().chain(CALLER_REFUSALS) {
        operation = operation.refusal(*code, *category);
    }
    operation
}

/// The recovery operations over `stores`, for `host`'s principals.
pub(crate) fn contribute(
    contribution: Contribution,
    stores: CollaborationStores,
    host: Arc<dyn OperationsHost>,
) -> Contribution {
    let context = Arc::new(Context { stores, host });
    let stores_context = context.clone();
    let verify_context = context.clone();
    let export_context = context.clone();
    let audit_context = context.clone();
    let quarantine_context = context.clone();
    let repair_context = context.clone();
    let reindex_context = context.clone();
    let reset_context = context;
    contribution
        .typed_operation(
            declared::<Nothing, Stores>(
                "clerkenwell.recovery.stores",
                "The names of the application's stores.",
                Effect::Query,
                RECOVERY_READ,
            )
            .idempotent(true),
            move |call: Call, _: Nothing| {
                let context = stores_context.clone();
                async move {
                    principal(&*context.host, &call)?;
                    Ok(Stores {
                        stores: context
                            .stores
                            .stores()
                            .into_iter()
                            .map(|(name, _)| name)
                            .collect(),
                    })
                }
            },
        )
        .typed_operation(
            declared::<StoredDocument, Inspection>(
                "clerkenwell.recovery.verify",
                "Whether a document's stored data can be read, and what it holds.",
                Effect::Query,
                RECOVERY_READ,
            )
            .idempotent(true),
            move |call: Call, input: StoredDocument| {
                let context = verify_context.clone();
                async move {
                    principal(&*context.host, &call)?;
                    let service = context.store(&input.store)?;
                    Ok(Inspection::from(
                        service.verify(&CollaborationDocumentId::new(input.entity, input.id)),
                    ))
                }
            },
        )
        .typed_operation(
            declared::<StoredDocument, Exported>(
                "clerkenwell.recovery.export",
                "A document's stored envelope, as the store keeps it.",
                Effect::Query,
                RECOVERY_READ,
            )
            .idempotent(true),
            move |call: Call, input: StoredDocument| {
                let context = export_context.clone();
                async move {
                    let actor = principal(&*context.host, &call)?;
                    let service = context.store(&input.store)?;
                    let bytes = service
                        .export_for_recovery(
                            &actor,
                            &CollaborationDocumentId::new(input.entity, input.id),
                        )
                        .map_err(refusals::store)?;
                    Ok(Exported {
                        envelope_base64: BASE64.encode(bytes),
                    })
                }
            },
        )
        .typed_operation(
            declared::<AuditInput, Audit>(
                "clerkenwell.recovery.audit",
                "A store's recorded recovery requests and refused imports, oldest first.",
                Effect::Query,
                RECOVERY_READ,
            )
            .idempotent(true),
            move |call: Call, input: AuditInput| {
                let context = audit_context.clone();
                async move {
                    principal(&*context.host, &call)?;
                    let service = context.store(&input.store)?;
                    let document = match (input.entity, input.id) {
                        (Some(entity), Some(id)) => Some(CollaborationDocumentId::new(entity, id)),
                        (None, None) => None,
                        _ => {
                            return Err(refusals::invalid_params(
                                "An audit selects one document by its entity and id together.",
                                json!({ "store": input.store }),
                            ))
                        }
                    };
                    let records = service
                        .audit(&CollaborationAuditQuery {
                            document,
                            actor_id: input.actor,
                            from_unix_ms: input.from_unix_ms.map(u128::from),
                            until_unix_ms: input.until_unix_ms.map(u128::from),
                        })
                        .map_err(refusals::store)?;
                    Ok(Audit {
                        records: records.into_iter().map(audit_record).collect(),
                    })
                }
            },
        )
        .typed_operation(
            declared::<Reasoned, Evidence>(
                "clerkenwell.recovery.quarantine",
                "Keeps a document's stored envelope as evidence, and returns where the store kept it.",
                Effect::Administrative,
                RECOVERY_ADMIN,
            )
            .idempotent(false),
            move |call: Call, input: Reasoned| {
                let context = quarantine_context.clone();
                async move {
                    let actor = principal(&*context.host, &call)?;
                    let service = context.store(&input.store)?;
                    let evidence = service
                        .quarantine(
                            &actor,
                            &CollaborationDocumentId::new(input.entity, input.id),
                            &input.reason,
                        )
                        .map_err(refusals::store)?;
                    Ok(Evidence { evidence })
                }
            },
        )
        .typed_operation(
            declared::<Reasoned, Evidence>(
                "clerkenwell.recovery.repair",
                "Keeps a document's stored envelope as evidence, then drops its retained operations so the document reads from its checkpoint alone.",
                Effect::Administrative,
                RECOVERY_ADMIN,
            )
            .idempotent(false),
            move |call: Call, input: Reasoned| {
                let context = repair_context.clone();
                async move {
                    let actor = principal(&*context.host, &call)?;
                    let service = context.store(&input.store)?;
                    let evidence = service
                        .repair(
                            &actor,
                            &CollaborationDocumentId::new(input.entity, input.id),
                            &input.reason,
                        )
                        .map_err(refusals::store)?;
                    Ok(Evidence { evidence })
                }
            },
        )
        .typed_operation(
            declared::<StoreInput, Reindexed>(
                "clerkenwell.recovery.reindex",
                "Re-reads every document a store keeps, and fingerprints the redacted catalogue.",
                Effect::Administrative,
                RECOVERY_ADMIN,
            )
            .idempotent(true),
            move |call: Call, input: StoreInput| {
                let context = reindex_context.clone();
                async move {
                    let actor = principal(&*context.host, &call)?;
                    let service = context.store(&input.store)?;
                    let (documents, fingerprint) =
                        service.reindex(&actor).map_err(refusals::store)?;
                    Ok(Reindexed {
                        documents,
                        fingerprint,
                    })
                }
            },
        )
        .typed_operation(
            declared::<StoredDocument, Done>(
                "clerkenwell.recovery.reset",
                "Deletes a document, whatever its stored data holds.",
                Effect::Destructive,
                RECOVERY_RESET,
            )
            .idempotent(false),
            move |call: Call, input: StoredDocument| {
                let context = reset_context.clone();
                async move {
                    let actor = principal(&*context.host, &call)?;
                    let service = context.store(&input.store)?;
                    service
                        .reset(
                            &actor,
                            &CollaborationDocumentId::new(input.entity, input.id),
                        )
                        .map_err(refusals::store)?;
                    Ok(Done {})
                }
            },
        )
}

/// The stores the recovery operations recover, and whose principals run
/// them.
struct Context {
    stores: CollaborationStores,
    host: Arc<dyn OperationsHost>,
}

impl Context {
    /// The store `name`.
    fn store(&self, name: &str) -> Result<CollaborationService, Refusal> {
        self.stores.store(name).ok_or_else(|| {
            refusals::not_found(
                format!("No store is named \"{name}\"."),
                json!({ "store": name }),
            )
        })
    }
}

fn audit_record(record: CollaborationAuditRecord) -> AuditRecord {
    AuditRecord {
        timestamp_unix_ms: u64::try_from(record.timestamp_unix_ms)
            .expect("a timestamp in milliseconds fits 64 bits"),
        actor: record.actor.id,
        actor_kind: serde_json::to_value(record.actor.kind)
            .expect("an actor kind serialises")
            .as_str()
            .expect("an actor kind serialises as a string")
            .to_owned(),
        entity: record.entity,
        id: record.resource_id,
        event: serde_json::to_value(record.event).expect("an audit event serialises"),
    }
}

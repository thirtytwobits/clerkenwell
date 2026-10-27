//! The tasks as a fixture of Clapham's conformance harness.
//!
//! Every probe starts from the same stored tasks: a [`Seed`], minted once and
//! handed to each process through the file [`SEED_VARIABLE`] names. A read
//! of them gives the same result through every front end. An edit or a
//! creation is written under peers the store allocates afresh, so its case
//! compares what it wrote and leaves out the etag, version and frontier it
//! mints.

use std::collections::BTreeMap;
use std::path::Path;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clapham::OAuthContext;
use clapham_conformance::{Case, Fixture};
use clerkenwell_axum::ProjectionServer;
use clerkenwell_events::{ActorKind, Principal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::operations::{configured, ACTOR_KIND, AUDIENCE, ISSUER, PRIVATE_TASK, SEEDED};
use crate::{task, Tasks, COMMAND_LIMIT, PUBLICATION_WINDOW, TASK};

/// The environment variable naming the file a fixture's seed is kept in.
pub const SEED_VARIABLE: &str = "TASKS_SEED";

/// Every scope the fixture's operations require.
pub const EVERY_SCOPE: &[&str] = &[
    clerkenwell_ops::DOCUMENTS_READ,
    clerkenwell_ops::DOCUMENTS_WRITE,
    clerkenwell_ops::RECOVERY_READ,
    clerkenwell_ops::RECOVERY_ADMIN,
    clerkenwell_ops::RECOVERY_RESET,
    "tasks:read",
    "tasks:write",
];

/// The stored envelope of each seeded task, by its id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seed {
    pub envelopes: BTreeMap<String, String>,
}

impl Seed {
    /// The envelopes of freshly seeded tasks.
    pub fn mint() -> Self {
        let tasks = Tasks::seeded(SEEDED);
        let seeder = Principal::new("seeder", ActorKind::System);
        let envelopes = SEEDED
            .iter()
            .map(|(id, _)| {
                let envelope = tasks
                    .service()
                    .export_for_recovery(&seeder, &task(id))
                    .expect("a seeded task is stored");
                ((*id).to_owned(), BASE64.encode(envelope))
            })
            .collect();
        Self { envelopes }
    }

    /// The seed kept in the file [`SEED_VARIABLE`] names, or a freshly minted
    /// one where it names none.
    pub fn from_environment() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        match std::env::var_os(SEED_VARIABLE) {
            Some(path) => Ok(serde_json::from_slice(&std::fs::read(path)?)?),
            None => Ok(Self::mint()),
        }
    }

    /// Keeps the seed in the file `path`.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, serde_json::to_vec(self).expect("a seed serialises"))
    }

    /// Tasks holding the seeded envelopes.
    pub fn tasks(&self) -> Tasks {
        let envelopes: Vec<(&str, Vec<u8>)> = self
            .envelopes
            .iter()
            .map(|(id, envelope)| {
                let bytes = BASE64
                    .decode(envelope)
                    .expect("a seeded envelope is base64");
                (id.as_str(), bytes)
            })
            .collect();
        Tasks::stored(envelopes.iter().map(|(id, bytes)| (*id, bytes.as_slice())))
    }
}

/// A grant to `subject`, acting as a person, with `scopes`.
pub fn grant(subject: &str, scopes: &[&str]) -> OAuthContext {
    OAuthContext::from_verified(ISSUER, AUDIENCE, subject, scopes.iter().copied())
        .with_claim(ACTOR_KIND, json!("human"))
}

fn case(operation: &str, input: Value) -> Case {
    let mut case = Case::new(operation, input);
    case.auth = grant("person", EVERY_SCOPE);
    case
}

/// What an edit wrote: the document without the etag its derived member
/// mints, its generation, and whether it was a retry.
fn written(result: &Value) -> Value {
    let mut document = result["document"].clone();
    if let Some(document) = document.as_object_mut() {
        document.remove("etag");
    }
    json!({
        "document": document,
        "generation": result["generation"],
        "duplicate": result["duplicate"],
    })
}

/// The tasks' fixture over `seed`: its cases, and a grant without a scope,
/// which the scope gate refuses every operation but describe.
pub fn fixture(seed: Seed) -> Fixture {
    let tasks = seed.tasks();
    let read = |id: &str| {
        tasks
            .service()
            .read_document(TASK, &task(id))
            .expect("a seeded task reads")
            .expect("a seeded task is stored")
    };
    let first = read("task-1");
    let second = read("task-2");
    let mut renamed = first.document.clone();
    renamed["title"] = json!("Write the summary");
    let document = |id: &str| json!({ "entity": "Task", "id": id });
    let stored = |id: &str| json!({ "store": "tasks", "entity": "Task", "id": id });
    let mut cases = vec![
        case("clerkenwell.document.list", json!({ "entity": "Task" })),
        case("clerkenwell.document.list", json!({ "entity": "Board" })),
        case(
            "clerkenwell.document.list",
            json!({ "entity": "Task", "store": { "shard": "a" } }),
        ),
        case("clerkenwell.document.get", document("task-1")),
        case("clerkenwell.document.get", document("task-9")),
        case("clerkenwell.document.get", document(PRIVATE_TASK)),
        case(
            "clerkenwell.document.edit",
            json!({
                "entity": "Task", "id": "task-1", "operation_id": "rename",
                "etag": first.summary.etag, "document": renamed,
            }),
        )
        .compare(written),
        case(
            "clerkenwell.document.edit",
            json!({
                "entity": "Task", "id": "task-1", "operation_id": "stale",
                "etag": "loro:stale", "document": { "title": "Stale" },
            }),
        ),
        case(
            "clerkenwell.document.editText",
            json!({
                "entity": "Task", "id": "task-2", "operation_id": "annotate",
                "field_path": "notes", "frontier_base64": second.accepted_frontier_base64,
                "read": "", "replacement": "Chapter one.",
            }),
        )
        .compare(written),
        case(
            "clerkenwell.document.editText",
            json!({
                "entity": "Task", "id": "task-9", "operation_id": "absent",
                "field_path": "notes", "frontier_base64": "AA==", "read": "",
                "replacement": "Absent.",
            }),
        ),
        case("clerkenwell.recovery.stores", json!({})),
        case("clerkenwell.recovery.verify", stored("task-1")),
        case(
            "clerkenwell.recovery.verify",
            json!({ "store": "absent", "entity": "Task", "id": "task-1" }),
        ),
        case("clerkenwell.recovery.export", stored("task-1")),
        case("clerkenwell.recovery.export", stored("task-9")),
        case("clerkenwell.recovery.audit", json!({ "store": "tasks" })),
        case(
            "clerkenwell.recovery.audit",
            json!({ "store": "tasks", "entity": "Task" }),
        ),
        case(
            "clerkenwell.recovery.quarantine",
            json!({ "store": "tasks", "entity": "Task", "id": "task-1", "reason": "inspect" }),
        ),
        case(
            "clerkenwell.recovery.repair",
            json!({ "store": "tasks", "entity": "Task", "id": "task-2", "reason": "repair" }),
        ),
        case("clerkenwell.recovery.reindex", json!({ "store": "tasks" })),
        case("clerkenwell.recovery.reindex", json!({ "store": "absent" })),
        case("clerkenwell.recovery.reset", stored("task-1")),
        case("tasks.tasks.list", json!({})),
        case("tasks.task.create", json!({ "title": "Plan the trip" })).compare(|result| {
            json!({
                "modification": result["modification"],
                "task_id": result["result"]["task_id"],
            })
        }),
        case("tasks.task.create", json!({ "title": "" })),
        case("tasks.task.closeAll", json!({})),
    ];
    let mut owner = case("clerkenwell.document.get", document(PRIVATE_TASK));
    owner.auth = grant("owner", EVERY_SCOPE);
    cases.push(owner);
    let mut anonymous = case("clerkenwell.recovery.stores", json!({}));
    anonymous.auth =
        OAuthContext::from_verified(ISSUER, AUDIENCE, "person", EVERY_SCOPE.iter().copied());
    cases.push(anonymous);
    let mut reader = case("tasks.task.closeAll", json!({}));
    reader.auth = grant("reader", &["tasks:read"]);
    cases.push(reader);
    let mut fixture = Fixture::configured(
        move || {
            configured(ProjectionServer::new(
                seed.tasks(),
                PUBLICATION_WINDOW,
                COMMAND_LIMIT,
            ))
        },
        cases,
    );
    fixture.denied_auth = grant("denied", &[]);
    fixture
}

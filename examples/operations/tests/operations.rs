//! Clerkenwell's operations over the tasks, called through the composed set
//! as every front end calls them.

use std::sync::Arc;
use std::time::Duration;

use clapham::{
    Bounds, Cancellation, Category, ComposeOptions, ComposedSet, CompositionCode, Configuration,
    Evidence, ExecutionMode, Exposure, OAuthContext, Outcome, RecordSink, Refusal,
};
use clerkenwell_axum::ProjectionServer;
use clerkenwell_events::{ActorKind, Principal};
use clerkenwell_example_operations::operations::{
    configured, server, TasksOperations, ACTOR_KIND, AUDIENCE, ISSUER, PRIVATE_TASK,
};
use clerkenwell_example_operations::Tasks;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

/// Every scope Clerkenwell's operations and the tasks' require.
const EVERY_SCOPE: &[&str] = &[
    clerkenwell_ops::DOCUMENTS_READ,
    clerkenwell_ops::DOCUMENTS_WRITE,
    clerkenwell_ops::RECOVERY_READ,
    clerkenwell_ops::RECOVERY_ADMIN,
    clerkenwell_ops::RECOVERY_RESET,
    "tasks:read",
    "tasks:write",
];

/// A grant to `subject`, acting as a person, with `scopes`.
fn grant(subject: &str, scopes: &[&str]) -> OAuthContext {
    OAuthContext::from_verified(ISSUER, AUDIENCE, subject, scopes.iter().copied())
        .with_claim(ACTOR_KIND, json!("human"))
}

fn person() -> OAuthContext {
    grant("person", EVERY_SCOPE)
}

fn compose(configuration: Configuration) -> ComposedSet {
    configuration
        .compose_with_options(
            Bounds::default(),
            ComposeOptions {
                execution: ExecutionMode::Daemon,
                ..Default::default()
            },
        )
        .expect("the tasks compose")
}

/// The tasks' operations over `server`.
fn composed(server: Arc<ProjectionServer<Tasks>>) -> ComposedSet {
    compose(configured(server))
}

async fn call(
    set: &ComposedSet,
    auth: OAuthContext,
    name: &str,
    input: Value,
) -> Result<Outcome, Refusal> {
    set.call(
        &set.all(),
        auth,
        name,
        input,
        RecordSink::default(),
        Cancellation::new(),
        false,
    )
    .await
}

async fn result(set: &ComposedSet, name: &str, input: Value) -> Value {
    call(set, person(), name, input)
        .await
        .unwrap_or_else(|refusal| panic!("{name} is refused: {refusal:?}"))
        .result
        .expect("a result")
}

async fn refusal(set: &ComposedSet, auth: OAuthContext, name: &str, input: Value) -> Refusal {
    call(set, auth, name, input)
        .await
        .expect_err("the call is refused")
}

fn task(id: &str) -> Value {
    json!({ "entity": "Task", "id": id })
}

#[tokio::test]
async fn every_document_operation_names_a_policy_a_composition_must_register() {
    let contributions = clerkenwell_ops::contribute(server(), Arc::new(TasksOperations))
        .expect("the tasks' operations");
    let mut configuration = Configuration::new(ISSUER, AUDIENCE);
    for contribution in contributions {
        let source = contribution.namespace.clone();
        let names: Vec<String> = contribution
            .operations
            .iter()
            .map(|(operation, _)| operation.name.to_string())
            .collect();
        configuration = configuration.provider(source.clone(), contribution);
        for name in names {
            configuration = configuration.expose(Exposure::new(source.clone(), name));
        }
    }

    let refused = configuration
        .compose_with_options(Bounds::default(), ComposeOptions::default())
        .expect_err("document operations without their policies");

    assert_eq!(refused.code, CompositionCode::InvalidConfiguration);
    assert!(
        refused.message.contains("clerkenwell.document"),
        "{}",
        refused.message
    );
}

#[tokio::test]
async fn a_grant_the_application_maps_to_no_principal_is_refused_before_the_operation_runs() {
    let set = composed(server());
    let anonymous =
        OAuthContext::from_verified(ISSUER, AUDIENCE, "person", EVERY_SCOPE.iter().copied());

    for (name, input) in [
        ("clerkenwell.document.get", task("task-1")),
        ("clerkenwell.recovery.stores", json!({})),
        ("tasks.tasks.list", json!({})),
    ] {
        let refused = refusal(&set, anonymous.clone(), name, input).await;
        assert_eq!(refused.code, "unauthenticated", "{name}");
        assert_eq!(refused.category, Category::Refused, "{name}");
        assert_eq!(refused.evidence, Evidence::NotDispatched, "{name}");
    }
}

#[tokio::test]
async fn a_document_is_read_and_edited_from_the_etag_it_was_read_at() {
    let set = composed(server());
    let read = result(&set, "clerkenwell.document.get", task("task-1")).await;
    assert_eq!(read["document"]["title"], "Write the report");
    let mut document = read["document"].clone();
    document["title"] = json!("Write the summary");
    let mut edit = task("task-1");
    edit["operation_id"] = json!("rename");
    edit["etag"] = read["etag"].clone();
    edit["document"] = document;

    let edited = result(&set, "clerkenwell.document.edit", edit.clone()).await;
    let retried = result(&set, "clerkenwell.document.edit", edit.clone()).await;

    assert_eq!(edited["document"]["title"], "Write the summary");
    assert_eq!(edited["duplicate"], false);
    assert_eq!(retried["duplicate"], true);
    assert_eq!(retried["etag"], edited["etag"]);
    let mut stale = edit;
    stale["operation_id"] = json!("rename-again");
    let refused = refusal(&set, person(), "clerkenwell.document.edit", stale).await;
    assert_eq!(refused.code, "collaboration_revision");
    assert_eq!(refused.category, Category::Conflict);
    let details = refused.details.expect("the accepted document");
    assert_eq!(details["current"], edited["document"]);
    assert_eq!(details["current_etag"], edited["etag"]);
}

#[tokio::test]
async fn an_edit_the_application_refuses_is_refused_as_validation_failed() {
    let set = composed(server());
    let read = result(&set, "clerkenwell.document.get", task("task-1")).await;
    let mut document = read["document"].clone();
    document["title"] = json!("");
    let mut edit = task("task-1");
    edit["operation_id"] = json!("untitle");
    edit["etag"] = read["etag"].clone();
    edit["document"] = document;

    let refused = refusal(&set, person(), "clerkenwell.document.edit", edit).await;

    assert_eq!(refused.code, "validation_failed");
    assert_eq!(refused.category, Category::InvalidInput);
}

#[tokio::test]
async fn concurrent_text_edits_from_one_frontier_merge() {
    let set = composed(server());
    let read = result(&set, "clerkenwell.document.get", task("task-2")).await;
    let replace = |operation_id: &str, replacement: &str| {
        let mut edit = task("task-2");
        edit["operation_id"] = json!(operation_id);
        edit["field_path"] = json!("notes");
        edit["frontier_base64"] = read["accepted_frontier_base64"].clone();
        edit["read"] = json!("");
        edit["replacement"] = json!(replacement);
        edit
    };

    result(
        &set,
        "clerkenwell.document.editText",
        replace("first", "Chapter one."),
    )
    .await;
    let merged = result(
        &set,
        "clerkenwell.document.editText",
        replace("second", "Chapter two."),
    )
    .await;

    let notes = merged["document"]["notes"].as_str().expect("notes");
    assert!(
        notes.contains("Chapter one.") && notes.contains("Chapter two."),
        "{notes}"
    );
}

#[tokio::test]
async fn the_private_task_is_reached_only_by_its_owner() {
    let set = composed(server());

    let refused = refusal(
        &set,
        person(),
        "clerkenwell.document.get",
        task(PRIVATE_TASK),
    )
    .await;
    let owned = call(
        &set,
        grant("owner", EVERY_SCOPE),
        "clerkenwell.document.get",
        task(PRIVATE_TASK),
    )
    .await
    .expect("the owner reads it");

    assert_eq!(refused.code, "forbidden");
    assert_eq!(refused.category, Category::Refused);
    assert_eq!(refused.evidence, Evidence::NotDispatched);
    assert_eq!(
        owned.result.expect("a result")["document"]["title"],
        "Plan the surprise"
    );
}

#[tokio::test]
async fn each_projection_mutation_and_command_is_an_operation_under_the_definition_s_namespace() {
    let set = composed(server());
    let names: Vec<String> = set
        .operations()
        .map(|operation| operation.name.to_string())
        .collect();
    assert!(names.contains(&"tasks.tasks.list".to_owned()), "{names:?}");
    assert!(names.contains(&"tasks.task.create".to_owned()), "{names:?}");
    assert!(
        names.contains(&"tasks.task.closeAll".to_owned()),
        "{names:?}"
    );
    assert!(
        !names.contains(&"tasks.tasks.authoringState".to_owned()),
        "{names:?}"
    );
    assert!(
        !names.contains(&"tasks.task.importUpdate".to_owned()),
        "{names:?}"
    );

    let created = result(
        &set,
        "tasks.task.create",
        json!({ "title": "Plan the trip" }),
    )
    .await;
    let closed = result(&set, "tasks.task.closeAll", json!({})).await;
    let listed = result(&set, "tasks.tasks.list", json!({})).await;

    let task_id = created["result"]["task_id"].clone();
    assert_eq!(closed["result"]["closed"], 3);
    let tasks = listed["snapshot"]["tasks"].as_array().expect("tasks");
    assert!(
        tasks.iter().any(|task| task["task_id"] == task_id),
        "{listed}"
    );
    assert!(
        tasks.iter().all(|task| task["status"] == "closed"),
        "{listed}"
    );
    assert!(listed["modification"].is_u64(), "{listed}");
}

#[tokio::test]
async fn the_generated_operations_leave_the_private_task_out() {
    let set = composed(server());

    let listed = result(&set, "tasks.tasks.list", json!({})).await;
    result(&set, "tasks.task.closeAll", json!({})).await;
    let private = call(
        &set,
        grant("owner", EVERY_SCOPE),
        "clerkenwell.document.get",
        task(PRIVATE_TASK),
    )
    .await
    .expect("the owner reads it");

    let tasks = listed["snapshot"]["tasks"].as_array().expect("tasks");
    assert!(
        tasks.iter().all(|task| task["task_id"] != PRIVATE_TASK),
        "{listed}"
    );
    assert_eq!(
        private.result.expect("a result")["document"]["status"],
        "open"
    );
}

#[tokio::test]
async fn a_whole_document_edit_is_idempotent_and_a_text_edit_is_not() {
    let set = composed(server());
    let idempotent = |name: &str| {
        set.operations()
            .find(|operation| operation.name.as_str() == name)
            .unwrap_or_else(|| panic!("{name}"))
            .idempotent
    };

    assert!(idempotent("clerkenwell.document.edit"));
    assert!(!idempotent("clerkenwell.document.editText"));
}

#[tokio::test]
async fn an_edit_retried_once_its_operation_is_no_longer_retained_is_refused_as_stale() {
    let set = composed(server());
    let rename = |read: &Value, operation_id: &str, title: &str| {
        let mut document = read["document"].clone();
        document["title"] = json!(title);
        let mut edit = task("task-1");
        edit["operation_id"] = json!(operation_id);
        edit["etag"] = read["etag"].clone();
        edit["document"] = document;
        edit
    };
    let read = result(&set, "clerkenwell.document.get", task("task-1")).await;
    let first = rename(&read, "first", "First");
    result(&set, "clerkenwell.document.edit", first.clone()).await;
    for later in 0..clerkenwell_example_operations::COMMIT_POLICY.retained_operations {
        let read = result(&set, "clerkenwell.document.get", task("task-1")).await;
        let title = format!("Later {later}");
        result(
            &set,
            "clerkenwell.document.edit",
            rename(&read, &title, &title),
        )
        .await;
    }
    let before = result(&set, "clerkenwell.document.get", task("task-1")).await;

    let refused = refusal(&set, person(), "clerkenwell.document.edit", first).await;

    assert_eq!(refused.code, "collaboration_revision");
    let after = result(&set, "clerkenwell.document.get", task("task-1")).await;
    assert_eq!(after, before, "the retry changed nothing");
}

#[tokio::test]
async fn an_edit_under_a_blank_operation_id_is_refused_as_invalid_input() {
    let set = composed(server());
    let first = result(&set, "clerkenwell.document.get", task("task-1")).await;
    let second = result(&set, "clerkenwell.document.get", task("task-2")).await;
    let mut edit = task("task-1");
    edit["operation_id"] = json!(" ");
    edit["etag"] = first["etag"].clone();
    edit["document"] = first["document"].clone();
    let mut text = task("task-2");
    text["operation_id"] = json!(" ");
    text["field_path"] = json!("notes");
    text["frontier_base64"] = second["accepted_frontier_base64"].clone();
    text["read"] = json!("");
    text["replacement"] = json!("Blank.");

    for (name, input) in [
        ("clerkenwell.document.edit", edit),
        ("clerkenwell.document.editText", text),
    ] {
        let refused = refusal(&set, person(), name, input).await;
        assert_eq!(refused.code, "invalid_collaboration_operation_id", "{name}");
        assert_eq!(refused.category, Category::InvalidInput, "{name}");
    }
}

#[tokio::test]
async fn a_generated_operation_is_refused_to_a_grant_without_its_scope() {
    let set = composed(server());
    let reader = grant("reader", &["tasks:read"]);

    let refused = refusal(
        &set,
        reader.clone(),
        "tasks.task.create",
        json!({ "title": "Plan" }),
    )
    .await;
    let listed = call(&set, reader, "tasks.tasks.list", json!({})).await;

    assert_eq!(refused.category, Category::Refused);
    assert_eq!(refused.evidence, Evidence::NotDispatched);
    assert!(listed.is_ok(), "{listed:?}");
}

#[tokio::test]
async fn a_recovery_operation_is_refused_to_a_grant_without_its_scope() {
    let set = composed(server());
    let operator = grant(
        "operator",
        &[
            clerkenwell_ops::RECOVERY_READ,
            clerkenwell_ops::RECOVERY_ADMIN,
        ],
    );
    let reset = json!({ "store": "tasks", "entity": "Task", "id": "task-1" });

    let refused = refusal(
        &set,
        operator.clone(),
        "clerkenwell.recovery.reset",
        reset.clone(),
    )
    .await;
    let stores = call(&set, operator, "clerkenwell.recovery.stores", json!({}))
        .await
        .expect("the stores");
    result(&set, "clerkenwell.recovery.reset", reset).await;
    let absent = refusal(&set, person(), "clerkenwell.document.get", task("task-1")).await;

    assert_eq!(refused.category, Category::Refused);
    assert_eq!(refused.evidence, Evidence::NotDispatched);
    assert_eq!(
        stores.result.expect("a result"),
        json!({ "stores": ["tasks"] })
    );
    assert_eq!(absent.code, "collaboration_document_not_found");
}

#[tokio::test]
async fn a_held_workspace_is_recovered_without_a_server() {
    let tasks = Tasks::seeded(&[("task-1", "Write the report")]);
    let authoring = clerkenwell_session::ProjectionHost::authoring_states(&tasks)
        .expect("the tasks' authoring states")
        .clone();
    let set = clapham::compose_with_options(
        vec![clerkenwell_ops::contribute_held(
            authoring,
            Arc::new(TasksOperations),
        )],
        Bounds::default(),
        ComposeOptions::default(),
    )
    .expect("the recovery operations compose");

    let verified = result(
        &set,
        "clerkenwell.recovery.verify",
        json!({ "store": "tasks", "entity": "Task", "id": "task-1" }),
    )
    .await;

    assert_eq!(verified["valid"], true);
    assert_eq!(verified["id"], "task-1");
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next(socket: &mut Socket) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a frame in time")
            .expect("an open connection")
            .expect("a frame");
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).expect("a JSON frame");
        }
    }
}

async fn subscribe(socket: &mut Socket, id: u64, projection: &str, params: Value) {
    socket
        .send(Message::Text(
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "projection.subscribe",
                "params": { "projection": projection, "params": params },
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("a sent frame");
    assert_eq!(next(socket).await["id"], json!(id));
    let snapshot = next(socket).await;
    assert_eq!(snapshot["params"]["kind"], "snapshot", "{snapshot}");
}

#[tokio::test]
async fn a_subscriber_takes_the_change_of_an_edit_and_of_a_mutation_made_through_the_operations() {
    let server = server();
    let set = composed(server.clone());
    let router = server.clone().router("/projections", |_| {
        Some(Principal::new("subscriber", ActorKind::Human))
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let address = listener.local_addr().expect("an address");
    tokio::spawn(async move { axum::serve(listener, router).await });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/projections"))
        .await
        .expect("a connection");
    subscribe(&mut socket, 1, "tasks.list", json!({})).await;
    subscribe(
        &mut socket,
        2,
        "tasks.authoringState",
        json!({ "task_id": "task-1" }),
    )
    .await;

    result(
        &set,
        "tasks.task.create",
        json!({ "title": "Plan the trip" }),
    )
    .await;
    let listed = next(&mut socket).await;
    let read = result(&set, "clerkenwell.document.get", task("task-1")).await;
    let mut document = read["document"].clone();
    document["title"] = json!("Write the summary");
    let mut edit = task("task-1");
    edit["operation_id"] = json!("rename");
    edit["etag"] = read["etag"].clone();
    edit["document"] = document;
    result(&set, "clerkenwell.document.edit", edit).await;
    let edited = [next(&mut socket).await, next(&mut socket).await];

    assert_eq!(listed["params"]["kind"], "patch", "{listed}");
    assert!(
        listed["params"]["patch"]["value"]["tasks"]
            .as_array()
            .is_some_and(|tasks| tasks.iter().any(|task| task["title"] == "Plan the trip")),
        "{listed}"
    );
    // The edit reaches the task list, as the store announced it, and the
    // task's authoring state.
    let renamed = |frame: &Value| {
        frame["params"]["patch"]["value"]["tasks"]
            .as_array()
            .is_some_and(|tasks| {
                tasks
                    .iter()
                    .any(|task| task["title"] == "Write the summary")
            })
    };
    assert!(edited.iter().any(renamed), "{edited:?}");
    assert!(
        edited
            .iter()
            .all(|frame| frame["params"]["kind"] == "patch"),
        "{edited:?}"
    );
}

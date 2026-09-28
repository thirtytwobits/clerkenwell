//! Two clients editing one note through the notes server.

use std::time::Duration;

use clerkenwell_axum::ProjectionServer;
use clerkenwell_doc::LoroAuthoringDocument;
use clerkenwell_example_notes::server::{NotesServer, SERVER_WINDOWS};
use clerkenwell_example_notes::NOTE;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A client of the server, numbering its requests.
struct Client {
    socket: Socket,
    requests: u64,
}

impl Client {
    async fn connect(url: &str) -> Self {
        Self {
            socket: tokio_tungstenite::connect_async(url)
                .await
                .expect("a connection")
                .0,
            requests: 0,
        }
    }

    async fn frame(&mut self) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(5), self.socket.next())
            .await
            .expect("a frame in time")
            .expect("an open connection")
            .expect("a frame");
        serde_json::from_str(message.to_text().expect("a text frame")).expect("a JSON frame")
    }

    /// The response to a request; updates that arrive first are skipped.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.requests += 1;
        let id = self.requests;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.socket
            .send(Message::Text(request.to_string().into()))
            .await
            .expect("send");
        loop {
            let frame = self.frame().await;
            if frame["id"] == json!(id) {
                return frame;
            }
        }
    }

    async fn mutate(&mut self, mutation: &str, params: Value) -> Value {
        self.call(
            "projection.mutate",
            json!({ "mutation": mutation, "params": params }),
        )
        .await
    }

    /// Subscribes to a note's authoring state and returns the state its
    /// snapshot delivers.
    async fn subscribe(&mut self, note_id: &str) -> Value {
        let response = self
            .call(
                "projection.subscribe",
                json!({ "projection": "notes.authoringState", "params": { "note_id": note_id } }),
            )
            .await;
        let subscription_id = response["result"]["subscription_id"].clone();
        loop {
            let frame = self.frame().await;
            let update = &frame["params"];
            if update["subscription_id"] == subscription_id && update["kind"] == "snapshot" {
                return update["snapshot"]["value"].clone();
            }
        }
    }

    /// The next authoring state a patch delivers.
    async fn patched_state(&mut self) -> Value {
        loop {
            let frame = self.frame().await;
            if frame["params"]["kind"] == "patch" {
                return frame["params"]["patch"]["value"]["state"].clone();
            }
        }
    }
}

async fn start() -> (String, tempfile::TempDir) {
    let root = tempfile::tempdir().expect("a store directory");
    let server = ProjectionServer::new(NotesServer::new(root.path()), SERVER_WINDOWS);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a listener");
    let address = listener.local_addr().expect("an address");
    let router = server.router("/projections");
    tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
    (format!("ws://{address}/projections"), root)
}

/// A replica of the note an authoring state delivers.
fn replica(state: &Value) -> LoroAuthoringDocument {
    LoroAuthoringDocument::from_versioned_update_base64(
        NOTE,
        NOTE.schema_version,
        state["update_base64"].as_str().expect("an update"),
    )
    .expect("a replica")
}

/// The note a replica holds.
fn document(replica: &LoroAuthoringDocument) -> Value {
    replica.materialized_document("client").expect("a note")
}

/// Edits a replica of `state` and sends what it recorded.
async fn edit(
    client: &mut Client,
    state: &Value,
    operation_id: &str,
    change: impl FnOnce(&mut Value),
) -> (LoroAuthoringDocument, Value) {
    let mut replica = replica(state);
    let mut note = document(&replica);
    change(&mut note);
    replica.replace_document(&note).expect("an edit");
    let base = state["accepted_frontier_base64"]
        .as_str()
        .expect("a frontier");
    let response = client
        .mutate(
            "note.importLoroUpdate",
            json!({
                "note_id": state["note_id"],
                "operation_id": operation_id,
                "exchange_mode": "incremental",
                "base_frontier_base64": base,
                "update_base64": replica.export_incremental_update_base64(base).expect("an update"),
            }),
        )
        .await;
    (replica, response)
}

async fn create(client: &mut Client, title: &str) -> String {
    let created = client
        .mutate("note.create", json!({ "title": title }))
        .await;
    created["result"]["result"]["value"]["note_id"]
        .as_str()
        .expect("a note id")
        .to_owned()
}

#[tokio::test]
async fn an_edit_one_client_sends_reaches_another_clients_subscription() {
    let (url, _root) = start().await;
    let mut ada = Client::connect(&url).await;
    let mut grace = Client::connect(&url).await;
    let note_id = create(&mut ada, "Launch plan").await;
    ada.subscribe(&note_id).await;
    let state = grace.subscribe(&note_id).await;

    let (edited, response) = edit(&mut grace, &state, "grace-body", |note| {
        note["body"] = json!("Ship on Friday.");
    })
    .await;
    assert!(response.get("error").is_none(), "{response}");

    let delivered = ada.patched_state().await;
    assert_eq!(
        document(&replica(&delivered))["body"],
        document(&edited)["body"]
    );
}

#[tokio::test]
async fn a_refused_status_carries_what_its_writer_lacks_to_rebase() {
    let (url, _root) = start().await;
    let mut ada = Client::connect(&url).await;
    let mut grace = Client::connect(&url).await;
    let note_id = create(&mut ada, "Launch plan").await;
    let ada_state = ada.subscribe(&note_id).await;
    let grace_state = grace.subscribe(&note_id).await;
    let (_, accepted) = edit(&mut ada, &ada_state, "ada-status", |note| {
        note["status"] = json!("review");
    })
    .await;
    assert!(accepted.get("error").is_none(), "{accepted}");

    let (refused, response) = edit(&mut grace, &grace_state, "grace-status", |note| {
        note["status"] = json!("published");
    })
    .await;

    let error = &response["error"];
    assert_eq!(
        error["data"]["projection_error"]["code"], "conflict",
        "{response}"
    );
    assert_eq!(error["data"]["projection_error"]["operation"], "mutate");
    refused
        .import_versioned_update_base64(
            NOTE.schema_version,
            error["data"]["missing_update_base64"]
                .as_str()
                .expect("what the writer lacks"),
        )
        .expect("take what the writer lacks");
    let rebased_state = json!({
        "note_id": note_id,
        "accepted_frontier_base64": error["data"]["accepted_frontier_base64"],
        "update_base64": refused.export_update_base64().expect("the rebased replica"),
    });
    let (rebased, response) =
        edit(&mut grace, &rebased_state, "grace-status-rebased", |_| {}).await;
    assert!(response.get("error").is_none(), "{response}");
    let delivered = grace.patched_state().await;
    assert_eq!(
        document(&replica(&delivered))["status"],
        document(&rebased)["status"]
    );
}
